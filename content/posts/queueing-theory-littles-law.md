+++
title = "Queueing basics: Little's law and why latency explodes near 100% utilisation"
summary = "Why a server that is 90% busy is far slower than one that is 70% busy. Little's law for sizing pools and workers, the 1/(1 − utilisation) curve, hidden queues, load shedding and a capacity-planning example."
tags = ["performance","scalability","system-design"]
level = "intermediate"
date = 2026-10-02
+++

Your API runs fine all week with its servers about 70% busy. On Monday a newsletter goes out,
traffic rises by about 30%, and the servers are now about 90% busy. You expect requests to get a
little slower. Instead, latency roughly triples and the slowest requests start to time out. No code
changed. You have met **queueing**: the closer a resource gets to 100% busy, the faster waiting time
grows.

This article explains why, with light math: one law, one formula and a few tables.

## Some vocabulary first

Queueing theory studies any system where work arrives, maybe waits, and then gets served:

- A **server** is anything that does one piece of work at a time: a CPU core, a worker thread, a
  database connection, a message consumer. (Not a whole machine.)
- **Arrival rate** (λ, the Greek letter *lambda*): how many requests arrive per second.
- **Service time** (S): how long one request keeps a server busy, *not counting* waiting.
- **Latency** (W): waiting time + service time. This is what users feel.
- **Utilisation** (ρ, the Greek letter *rho*): the fraction of time a server is busy. For one server,
  ρ = λ × S. With 80 requests per second that take 10 ms each, ρ = 80 × 0.010 = 0.8, or 80%.

A system is **stable** only if utilisation stays below 100%. If work arrives faster than it can be
done, the queue grows without limit, however large your buffers are.

## Little's law: L = λ × W

In 1961 John D. C. Little published a proof (*A Proof for the Queuing Formula: L = λW*, in the
journal *Operations Research*) of a rule that holds for almost any stable system:

```text
   L          =        λ          ×        W
items inside      arrival rate        average time each item spends inside
(on average)
```

It does not care whether arrivals are random or regular, whether service times vary, or in what
order items are served. It needs long-run averages, a stable system, and consistent units (requests
per *second* with time in *seconds*). The "box" can be anything: a service, a pool, a queue.

| Situation | You know | Little's law gives |
|---|---|---|
| Requests in flight in an API | 500 req/s, 200 ms average latency | L = 500 × 0.2 = **100** requests inside at once |
| Database connections in use | 2,000 queries/s, 4 ms each | L = 2,000 × 0.004 = **8** connections busy |
| Background job workers | 300 jobs/s, 50 ms each | **15** workers busy on average |
| A message queue backlog | 6,000 messages waiting, consumers finish 100/s | W = 6,000 / 100 = **60 s** before a new message is handled |

Two practical uses follow:

1. **Sizing.** L is the *average* concurrency. You need more than that, because traffic comes in
   bursts and running at 100% is a disaster (next section). But L gives the order of magnitude:
   8 busy connections do not need a pool of 200 (see [connection pooling](/posts/connection-pooling)).
2. **Finding hidden waiting.** Measure requests in flight (L) where requests enter, for example at
   the load balancer, and measure throughput (λ). Then L / λ is the true average latency from that
   point on, including time in queues your application metrics don't see. If it is much larger
   than the latency your handler reports, requests wait somewhere before your code runs.

> [!NOTE]
> When a dependency gets slower (W goes up) and traffic stays the same, the number of requests inside
> (L) grows by the same factor. A database that slows from 5 ms to 50 ms per query needs ten times as
> many connections for the same traffic. That is how one slow dependency drains every pool upstream
> of it (see [resilience patterns](/posts/resilience-patterns)).

## Why latency explodes near 100%

Little's law describes averages, not *how long the queue gets*. For that you need a model. The
simplest useful one is called **M/M/1**, and it assumes:

- One server and one first-in, first-out queue with no size limit.
- Requests arrive **randomly and independently**, like customers walking into a shop.
- Service times are random too: most are short, a few are long. (Formally: Poisson arrivals and
  exponentially distributed service times.)

Under these assumptions the average latency is:

```text
W = S / (1 − ρ)
```

The factor **1 / (1 − ρ)** is the key. Here it is with a service time of 10 ms:

| Utilisation ρ | 1 / (1 − ρ) | Avg. wait in queue | Avg. latency | p99 latency (same model) |
|---|---|---|---|---|
| 50% | 2× | 10 ms | 20 ms | ~92 ms |
| 70% | 3.3× | 23 ms | 33 ms | ~150 ms |
| 80% | 5× | 40 ms | 50 ms | ~230 ms |
| 90% | 10× | 90 ms | 100 ms | ~460 ms |
| 95% | 20× | 190 ms | 200 ms | ~920 ms |
| 99% | 100× | 990 ms | 1,000 ms | ~4.6 s |

```text
average latency (10 ms of real work per request, each # = 10 ms)
50%  ##                                                    20 ms
70%  ###                                                   33 ms
80%  #####                                                 50 ms
90%  ##########                                           100 ms
95%  ####################                                 200 ms
99%  #################################################> 1,000 ms (off the chart)
```

Going from 50% to 80% adds 30 ms. Going from 90% to 99% adds 900 ms.

**Why?** Random arrivals come in clumps. Three requests may arrive in the same millisecond, then
nothing for 30 ms. During a clump a queue forms; during the quiet gaps the server drains it. At 50%
utilisation there is plenty of idle time to drain queues. At 95% there is almost none, so the queue
from one burst is still there when the next burst arrives.

Compare a perfectly regular system: one request exactly every 10 ms, each taking exactly 9 ms. That
is 90% utilisation with **no queue at all**. Queues come from **variability**; utilisation decides how
much it hurts. An approximation for a single server, Kingman's formula, captures both effects.
The average time spent waiting in the queue is roughly *ρ / (1 − ρ) × V × S*. Here V measures
variability: it is the average of the squared *coefficients of variation* (standard deviation
divided by mean) of the time between arrivals and of the service time. V is 0 when both are
perfectly regular, 1 in the random M/M/1 case (where the formula is exact), and above 1 when
arrivals or service times are burstier. The approximation is most accurate when the server is busy.

Real systems are often worse than M/M/1. Traffic is burstier than random (retry storms, cron jobs
at the top of the hour), and service times have long tails (garbage-collection pauses, slow queries).
Treat the table as an optimistic (best) case.

## Tail latency feels it first

In the M/M/1 model the p99 (the latency that 99% of requests beat) is about **4.6 times the
average** at every utilisation. At 90% busy, 1 request in 100 takes more than ~460 ms for 10 ms of
real work. Real systems often have an even larger gap.

Tails matter more than they seem because requests fan out. If a page calls 20 backends in parallel and
must wait for all of them, it is slow when **any** call is slow. If each call has a 1% chance of being
slower than its p99, the chance that at least one of 20 is slow is 1 − 0.99²⁰ ≈ **18%**. The
backend's "rare" p99 now hits almost one page view in five. Jeffrey Dean and Luiz André Barroso
describe this effect in *The Tail at Scale*. See
[observability](/posts/observability-logs-metrics-traces) for measuring percentiles.

## More workers per queue help, up to a point

With **c** workers sharing one queue (the M/M/c model), waiting is much shorter at the same
utilisation. Average wait in the queue, in multiples of the service time S:

| Workers sharing one queue | at 80% busy | at 90% busy | at 95% busy |
|---|---|---|---|
| 1 | 4.0 S | 9.0 S | 19 S |
| 2 | 1.8 S | 4.3 S | 9.3 S |
| 8 | 0.29 S | 0.88 S | 2.1 S |
| 32 | 0.03 S | 0.14 S | 0.44 S |

Two lessons:

- **Large shared pools can run hotter than small ones.** A single-threaded component is the worst
  row: a Node.js event loop, Redis's main command thread, or a hot row that every transaction locks.
- **One shared queue beats many separate queues.** Eight servers with their own queues, fed by random
  assignment, behave like eight copies of the first row. This is one reason "least connections" load
  balancing usually beats random assignment (see [load balancing](/posts/load-balancing-and-stateless-servers)).

The knee does not disappear with more workers; it only moves closer to 100%.

## Why systems need headroom

**Headroom** is capacity you deliberately leave unused at peak. You need it for:

1. **Latency.** The last 20% of utilisation is where latency multiplies.
2. **Bursts.** 60% CPU averaged over five minutes can hide many seconds at 100%.
3. **Failures.** When an instance or a whole zone dies, the survivors take its load.
4. **Deploys.** Rolling deploys remove capacity, and new instances start with cold caches.
5. **Feedback loops.** Slow responses cause client timeouts and retries, which raise the arrival rate
   when you have no spare capacity (see [retries and timeouts](/posts/retries-timeouts-and-idempotency)).
6. **Slow scaling.** New instances take minutes; queues fill in seconds (see [autoscaling](/posts/autoscaling)).

There is no universal target. For latency-sensitive services, a common starting point is to keep the
bottleneck resource at roughly 50–70% utilisation at expected peak, then adjust with load tests and
latency measurements. This is a rule of thumb, not a law. Batch processing cares about
throughput, not the wait of each item, so it can run close to 100% if the backlog drains in time.

## Hidden queues everywhere

A request passes through many queues before and after your code runs:

| Where | What waits there | What bounds it (examples) |
|---|---|---|
| Load balancer / proxy | Requests when backends are at their limit | HAProxy per-server `maxconn` and `maxqueue`, plus `timeout queue` |
| Kernel accept backlog | TCP connections your process has not `accept()`ed yet | `listen()` backlog (Tomcat calls it `acceptCount`), capped by `net.core.somaxconn` on Linux |
| Web server worker pool | Requests waiting for a free thread or process | Tomcat `maxThreads` and `maxConnections`; the queue of a Java `ThreadPoolExecutor` |
| Event loop (Node.js, asyncio) | Callbacks waiting for the single thread | No direct limit: watch event-loop lag |
| Connection pool | Handlers waiting for a database connection | Pool size and acquire timeout |
| Database | Queries waiting for CPU, disk or row locks | PostgreSQL `lock_timeout`, `statement_timeout` |
| Message queue | Messages waiting for a consumer | Max length or message TTL, number of consumers |
| Outgoing HTTP client | Calls waiting for a connection to another service | Per-host connection limit and timeouts |

Latency is the **sum of the waits in all of these**, plus the real work. Application metrics usually
start the clock when your handler begins, so the first queues are invisible. Compare load balancer
latency with application latency to find them.

> [!WARNING]
> Many of these queues are unbounded or very large by default. For example, Java's
> `Executors.newFixedThreadPool` uses an unbounded queue. An unbounded queue never rejects work; it
> only makes every request slower until clients give up, or until the process runs out of memory.

## Bounded queues and load shedding

During overload, an unbounded queue grows until every request waits longer than the client's
timeout. The server stays 100% busy, but only for clients that already gave up: **goodput** (useful,
completed work) falls towards zero.

The fix is to bound every queue and reject what does not fit, quickly and cheaply:

```text
               bounded queue (max 40)            8 workers
requests --> [r][r][r] ... [r][r] --> [w][w][w][w][w][w][w][w] --> responses
    |
    +-- queue full? reply 503 + Retry-After at once (cheap; a good client backs off)

workers skip any request whose deadline passed while it waited
```

**How big should the queue be?** Little's law again: max queue length ≈ throughput × the longest
wait you accept. Eight workers at 50 ms per request finish 160 requests per second. If a request may
wait at most 250 ms, the queue should hold about 160 × 0.25 = **40** requests.

```go
var ErrOverloaded = errors.New("overloaded")

var queue = make(chan Job, 40) // bounded: at most 40 jobs wait

func submit(j Job) error {
	select {
	case queue <- j:
		return nil
	default:
		return ErrOverloaded // caller returns 503 with Retry-After
	}
}
```

Also:

- **Drop expired work.** If a request waited longer than its deadline, don't start it.
- **Limit concurrency, not only rate** (see [rate limiting](/posts/rate-limiting)).
- **Shed by priority.** Reject background and batch traffic before checkout or login.
- **Limit time in the queue, not just length.** Facebook has described doing this in Ben Maurer's
  article *Fail at Scale* (ACM Queue, 2015). It uses ideas from CoDel ("Controlled Delay", an
  algorithm first designed for queues in network routers) and "adaptive LIFO" (serving the newest
  requests first when the queue is long).

## Worked example: capacity planning

A checkout service has these **measured** numbers:

- Peak traffic: 1,200 requests/second (the busiest minute of the busiest day, not a daily average).
- Each request keeps a worker busy for 50 ms on average and a database connection for 8 ms.
- Each instance runs 8 workers. Instances run in 3 availability zones.

**Step 1: average concurrency.** Little's law: 1,200 × 0.050 = **60 workers busy** at peak, or 7.5
instances at 100% utilisation. Never plan for that.

**Step 2: target utilisation.** Aim for 70% at peak: 60 / 0.7 ≈ 86 workers, so 11 instances (88 workers,
68% busy).

**Step 3: survive a zone failure.** If one zone of three dies, two-thirds of the fleet must carry the
peak. Accept up to 75% during the failure: the two remaining zones need 60 / 0.75 = 80 workers, or 10
instances. That means 5 per zone, **15 instances** in total (120 workers). Normally they are
60 / 120 = **50%** busy.

**Step 4: check the queues behind you.** Database connections busy at peak: 1,200 × 0.008 ≈ 10. A pool
of 5 per instance (75 in total) leaves plenty of room.

**Step 5: bound your own queue.** Each instance finishes 8 / 0.05 = 160 req/s. With a 250 ms maximum
queue wait, allow about 40 queued requests per instance, then return `503`.

**Step 6: verify.** Load-test one instance, raising traffic step by step, and find where p99 latency
bends upwards. Service time often grows with load (lock contention, garbage collection), so trust the
measurement over the model.

## Common mistakes

- **Planning with averages.** A daily average of 200 req/s says nothing about the peak second.
- **Watching only average latency.** The p99 degrades earlier and further.
- **Unbounded queues and huge pools "to be safe".** They hide the waiting.
- **Measuring latency only inside the handler**, which hides the accept backlog and worker queue.
- **Mixing units in Little's law.** 2,000 queries/s × 4 ms is 8, not 8,000.

## Further reading

- Google SRE Book: [Handling Overload](https://sre.google/sre-book/handling-overload/)
- AWS Builders' Library: [Using load shedding to avoid overload](https://aws.amazon.com/builders-library/using-load-shedding-to-avoid-overload/)
- AWS Builders' Library: [Avoiding insurmountable queue backlogs](https://aws.amazon.com/builders-library/avoiding-insurmountable-queue-backlogs/)
- Linux man page: [listen(2)](https://man7.org/linux/man-pages/man2/listen.2.html) (the accept backlog)
- Jeffrey Dean and Luiz André Barroso, *The Tail at Scale*, Communications of the ACM (2013)
- Ben Maurer, *Fail at Scale*, ACM Queue (2015)
- Mor Harchol-Balter, *Performance Modeling and Design of Computer Systems: Queueing Theory in Action* (book)
