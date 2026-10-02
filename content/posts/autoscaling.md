+++
title = "Autoscaling: adding capacity automatically (and safely)"
summary = "How autoscalers decide when to add or remove capacity: horizontal vs vertical, choosing the signal, Kubernetes HPA, VPA and node autoscalers, the scale-out delay, flapping, scale-to-zero, and the downstream and cost limits that keep it safe."
tags = ["scalability","devops","containers"]
level = "intermediate"
date = 2026-10-02
+++

Every weekday at 9:00, traffic to your API triples. At 9:04 the alerts fire, someone adds three
servers by hand, and by 9:15 things are calm again. At night the same ten servers sit almost idle,
and you pay for all of them. **Autoscaling** adds capacity when load rises and removes it when load
falls, with nobody at the keyboard. But a careless setup adds new ways to fail: it reacts too late,
it adds and removes servers in a loop, it opens more database connections than the database allows,
or it turns a bug into a large cloud bill. This article explains how autoscalers work and how to make
them safe.

## What an autoscaler is

An autoscaler is a **control loop**. Every few seconds it reads a metric, compares it with a target
you chose, and changes capacity to move the metric back towards the target.

```text
   +-------------+      +-------------------+      +-------------------+
   | metric      | ---> | autoscaler        | ---> | provisioner       |
   | (CPU, RPS,  |      | compare to target |      | starts pods / VMs |
   | queue size) |      | apply min / max   |      | (takes minutes)   |
   +-------------+      +-------------------+      +-------------------+
          ^                                                  |
          |                                                  v
          +------ load per instance drops <------ new instances get traffic
```

It can change capacity in two directions:

- **Horizontal** (scale out / in): change the **number** of instances. This needs stateless instances
  behind a load balancer, so that any instance can serve any request (see
  [load balancing and stateless servers](/posts/load-balancing-and-stateless-servers)). It is the main
  tool for web, API and worker tiers.
- **Vertical** (scale up / down): change the CPU and memory **of each** instance. It usually needs a
  restart and is limited by the biggest machine available. It is mostly used to find the right size
  for each instance.

## Choosing the signal

A good signal rises **before** users suffer, falls when load falls, and goes down when you add
instances.

| Signal | Good for | Watch out for |
|---|---|---|
| CPU utilisation | CPU-bound stateless services | Stays low if the app mostly waits on the database or other APIs |
| Memory | Almost never a good trigger | Often does not drop when load drops (caches, garbage collector), so you never scale back in |
| Requests per second per instance | APIs where requests cost about the same | You must know what one instance can handle: load-test it |
| In-flight requests (concurrency) | Slow or uneven requests | Needs instrumentation in the app or proxy |
| Queue depth or consumer lag | Background workers | Use backlog **per worker**, not the total |
| Latency | Alerts; a secondary signal | If a slow database causes it, more instances make it worse |

For workers, turn the queue into a target per worker. If one worker handles 10 messages per second
and a message may wait at most 60 seconds, each worker can own a backlog of 600. With 6,000 messages
waiting, you want 10 workers. AWS documents this "backlog per instance" approach for SQS workers.

Latency is tempting because users feel it, but it rises when your app is busy *and* when your database
is busy. Only the first is fixed by more instances. Scale on your instance's own work; alert on
latency.

## Kubernetes: three autoscalers that work together

### Horizontal Pod Autoscaler (HPA)

The HPA is built into [Kubernetes](/posts/docker-to-kubernetes). By default, every 15 seconds it computes:

```text
desiredReplicas = ceil( currentReplicas × currentMetricValue / targetMetricValue )
```

With 4 pods at 90% CPU and a target of 60%: ceil(4 × 90 / 60) = **6 pods**. If the ratio is within a
small tolerance (10% by default), it does nothing. With several metrics, it uses the largest result.

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: api
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: api
  minReplicas: 3                  # survive losing a pod or a zone
  maxReplicas: 30                 # cost guardrail and database-connection cap
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 60  # percent of the pod's CPU *request*
  behavior:
    scaleUp:
      policies:
        - type: Percent           # at most double every minute
          value: 100
          periodSeconds: 60
    scaleDown:
      stabilizationWindowSeconds: 300
      policies:
        - type: Pods              # remove at most 2 pods per minute
          value: 2
          periodSeconds: 60
```

- Utilisation is measured against the pod's CPU **request** (the CPU the scheduler reserves for it).
  If a container in the pod has no CPU request, the HPA cannot compute utilisation and takes no
  action for that metric.
- Without a `behavior` block, the HPA may add up to 100% more pods (or 4 pods, if that is more) every
  15 seconds. For scale-down it uses the 5-minute window, then may remove all extra pods at once. The
  policies above slow scale-up to "at most double per minute" and scale-down to 2 pods per minute.
  Check that your scale-up limit is not slower than your traffic can grow.
- CPU and memory metrics usually come from the **metrics-server** add-on. For other metrics you need
  an adapter (such as the Prometheus Adapter) or KEDA (below).
- Remove `replicas` from the Deployment manifest. Otherwise every `kubectl apply` resets the count the
  HPA chose. Be careful: if you simply delete the field and apply, Kubernetes can drop the Deployment
  to 1 replica (the default) once. The Kubernetes HPA documentation explains how to remove it safely.

### Vertical Pod Autoscaler (VPA)

The VPA is not part of core Kubernetes; you install it from the `kubernetes/autoscaler` project. It
watches real usage and recommends CPU and memory **requests**. Its modes range from `Off` (only
recommend) and `Initial` (set requests only when a pod is created) to modes that change running pods:
`Recreate` evicts pods so they restart with new values, and newer versions can also resize pods in
place. Many teams run it in `Off` mode and copy its recommendations by hand. Do not let the HPA and the
VPA act on the **same** resource (CPU or memory) for the same workload: each changes the number the
other reacts to, so they fight. The VPA project documents this limitation; using the VPA for memory
and the HPA for CPU, or the HPA on custom metrics, is allowed.

### Cluster Autoscaler and Karpenter

The HPA adds pods, but pods need **nodes** (machines). When no node has room, new pods stay
**Pending**, and a node autoscaler reacts:

```text
 [HPA]  CPU 90% > target 60%  ->  replicas 4 -> 6
   |
   v
 [scheduler]  no node has free CPU for 2 new pods  ->  pods stay "Pending"
   |
   v
 [Cluster Autoscaler / Karpenter]  sees Pending pods  ->  asks the cloud for a VM
   |
   v
 [new node]  pods scheduled -> image pulled -> app starts -> ready -> traffic
```

- **Cluster Autoscaler** (also from `kubernetes/autoscaler`) grows and shrinks predefined **node
  groups**, such as AWS Auto Scaling groups. By default, it removes a node when the pods on it request
  less than half of the node's capacity, those pods fit on other nodes, and this has been true for 10
  minutes.
- **Karpenter**, originally built by AWS and now also available for other clouds, creates nodes
  directly (not through node groups), picks instance types that fit the pending pods, and
  **consolidates** pods onto fewer or cheaper nodes.

Both decide from pod **requests**, not real usage. Requests far too high waste money; far too low,
and too many pods share one node, so they slow each other down or run out of memory.

## Virtual machines: auto scaling groups

Without Kubernetes, the same idea applies to VMs: AWS **Auto Scaling groups**, Google Cloud **managed
instance groups** and Azure **Virtual Machine Scale Sets**. You provide a template and a **minimum**,
**maximum** and **desired** size. The group replaces unhealthy instances and runs your policies. On
AWS, **target tracking** ("keep average CPU at 50%") is the usual start; **step scaling** adds more
instances the further a metric is past a threshold. An **instance warm-up** setting stops a new
instance's metrics from counting before it has started properly, and **lifecycle hooks** pause an
instance before it is terminated, so it can finish its work (for example, drain connections or upload
logs).

## The scale-out delay, and why you need headroom

New capacity is never instant. A typical sequence (illustrative timings; measure your own):

```text
 0:00  traffic starts rising
 0:30  averaged metric crosses the target; autoscaler asks for more pods
 0:45  no room on nodes -> node autoscaler asks the cloud for a VM
 2:00  VM booted and joined the cluster
 2:30  container image pulled (larger images take longer)
 3:00  app started; caches and connection pools warming up
 3:30  health checks pass -> first requests served
```

For minutes, the **existing** instances carry the extra load. Do the math: if traffic can grow 50%
while new capacity is on its way, instances that target 60% CPU reach 90% before help arrives. At an
80% target they would need 120%: overload, timeouts, retries, and even more load. Latency also grows
sharply as utilisation approaches 100% (see [queueing basics](/posts/queueing-theory-littles-law)).

So the target **is** your headroom; 50–70% CPU is a common starting point. You can also shorten the
delay:

- Small images and fast startup. Do heavy initialisation before reporting ready.
- Readiness probes (the check that decides whether a pod gets traffic) that pass only when the app
  can really serve.
- Capacity that is already booted: AWS **warm pools**, or low-priority placeholder pods that hold
  space on nodes and are evicted when real pods need it (the Cluster Autoscaler FAQ calls this
  overprovisioning).

## Flapping, cooldowns and stabilisation windows

**Flapping** is an autoscaler that cannot settle: it adds instances, load per instance drops, it
removes instances, load per instance rises, and it adds them again. Every cycle breaks connections and
empties caches. The fixes add **patience** to the loop:

- **Tolerance**: ignore small differences from the target.
- **Stabilisation window**: before scaling down, take the highest recommendation of the last N
  minutes. The HPA uses 5 minutes for scale-down by default, and no window for scale-up.
- **Cooldown**: wait after a scaling action before the next one. On AWS, simple scaling policies use a
  default cooldown of 300 seconds; AWS now recommends target tracking or step scaling instead, which
  rely on instance warm-up rather than cooldowns.
- **Asymmetry**: scale out fast, scale in slowly. Spare capacity costs a little money; missing
  capacity costs errors.

Scaling in is a small deploy. Handle `SIGTERM`, stop taking new work, finish in-flight requests, then
exit. In Kubernetes, a **PodDisruptionBudget** limits how many pods of one application may be down at
the same time during planned evictions, such as when a node autoscaler removes a node.

## Scheduled and predictive scaling

Reactive scaling is always a little late. When you **know** a peak is coming:

- **Scheduled scaling** raises the minimum before business hours, a marketing email or a sale, and
  lowers it afterwards: scheduled actions on an Auto Scaling group, KEDA's cron scaler, or a CronJob
  that changes the HPA's `minReplicas`.
- **Predictive scaling** learns daily and weekly patterns from history and adds capacity before the
  usual peak. AWS EC2 Auto Scaling and Google Cloud managed instance groups both offer it.

Both raise the floor; keep reactive scaling for surprises.

## Scale to zero and cold starts

A nightly report worker or a staging environment is idle most of the day. Running zero instances
while idle saves money.

- **KEDA** (Kubernetes-based Event Driven Autoscaling) scales a Deployment from 0 to 1 when an event
  source has work (a queue, a Kafka topic, a Prometheus query), and creates an HPA for 1 to N. No messages,
  no pods.
- **HTTP scale-to-zero** needs something to hold the first request while an instance starts: Knative
  Serving's activator, the KEDA HTTP add-on, or serverless platforms such as AWS Lambda and Google
  Cloud Run.

The price is the **cold start**: the first request after an idle period waits for an instance to
start, from a fraction of a second to many seconds. Workers rarely care; user-facing APIs often do.
Keep one warm instance (Cloud Run's minimum instances, Lambda's provisioned concurrency) and make
startup fast.

## Downstream limits: autoscaling moves the bottleneck

More web servers do not add database capacity. They add **database connections**:

```text
 total connections = replicas × pool size per replica
   normal:    5 pods × 20 = 100
   peak:     30 pods × 20 = 600     (PostgreSQL's default max_connections is 100)
```

The autoscaler opens the most connections exactly when the database is busiest. If the database is
the real bottleneck, more app instances only send more queries to it, and everything gets slower.

- Derive `maxReplicas` from the downstream budget: replicas × pool size must stay below the
  connections the database allows **for this service**. Leave room for rolling deploys, when old and
  new pods run at the same time.
- Use small pools and an external pooler such as PgBouncer (see
  [connection pooling](/posts/connection-pooling)).
- Remember rate-limited third-party APIs and other internal services.
- Shed load at the edge when demand exceeds what the whole system can serve (see
  [rate limiting](/posts/rate-limiting) and [resilience patterns](/posts/resilience-patterns)).

## Max limits are cost guardrails

Without a maximum, an autoscaler will follow a bot attack, a retry storm, or a bug that keeps the CPU
at 100%, and you find out from the cloud bill. Set limits at every layer: `maxReplicas` on each HPA
or KEDA object, the maximum size of each Auto Scaling group, node limits (Karpenter NodePools can cap
total CPU and memory; Cluster Autoscaler has maximum node counts), and cloud budget alerts.

Then **alert when a workload stays at its maximum** for more than a few minutes. Either growth is
real (raise the limit on purpose) or something is wrong (find it).

## Load-test your scaling policy

A scaling policy only runs on your busiest days, so test it first. With a tool such as k6, Gatling,
Locust or JMeter, test the *shape* of load, not just a steady rate:

- **Step**: jump to 2× load. How long until new instances serve traffic?
- **Spike**: 5× for two minutes. Do errors stay acceptable while capacity arrives?
- **Ramp down**: back to normal. Is scale-in slow and clean, with no 5xx errors?

Watch the replica count (flapping?), p99 latency (the latency 99% of requests stay under), errors and
database connections. Look for limits you forgot: cloud quotas, free IP addresses in subnets, the
database. Write down the measured scale-out delay; it tells you how much headroom you need.

## When not to autoscale

- **Steady traffic.** A fixed size with headroom is simpler.
- **Spikes shorter than your scale-out delay.** If a peak lasts 60 seconds and capacity takes 3
  minutes, provision for the peak instead.
- **Databases and brokers.** Adding a node means copying and rebalancing data. Use a managed
  product's built-in scaling (for example Amazon Aurora Serverless v2) rather than building your own.
- **A slow query.** Autoscaling multiplies inefficiency. Fix the code first.

## Checklist and common mistakes

- [ ] Instances are stateless, shut down gracefully, and report ready honestly.
- [ ] Kubernetes resource requests are set from measured usage.
- [ ] The signal measures the instance's own work, with headroom (e.g. 60% CPU).
- [ ] Minimum of 2–3 across zones; maximum derived from downstream limits and budget.
- [ ] Fast scale-up; slow, rate-limited scale-down.
- [ ] Known peaks have scheduled scaling.
- [ ] Alerts for "at maximum", long-Pending pods, and connection-pool wait time.
- [ ] The policy has been load-tested with step, spike and ramp-down patterns.

Common mistakes: scaling on memory; HPA and VPA on the same metric; `replicas` in the manifest
fighting the HPA; a minimum of 1, so one crash is an outage; readiness that passes before warm-up;
and no alert at the maximum.

## Further reading

- Kubernetes docs: [Horizontal Pod Autoscaling](https://kubernetes.io/docs/concepts/workloads/autoscaling/horizontal-pod-autoscale/)
- [kubernetes/autoscaler](https://github.com/kubernetes/autoscaler): Cluster Autoscaler and Vertical Pod Autoscaler, with their FAQs
- [Karpenter documentation](https://karpenter.sh/)
- [KEDA documentation](https://keda.sh/)
- AWS: [What is Amazon EC2 Auto Scaling?](https://docs.aws.amazon.com/autoscaling/ec2/userguide/what-is-amazon-ec2-auto-scaling.html)
