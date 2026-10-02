+++
title = "Resilience patterns: circuit breakers, bulkheads, load shedding and graceful degradation"
summary = "How one slow dependency takes down a whole system — and the patterns that contain the damage: timeouts, circuit breakers, bulkheads, load shedding, backpressure and fallbacks."
tags = ["reliability", "distributed-systems", "microservices"]
level = "advanced"
date = 2026-10-02
+++

Systems rarely fail because one component crashes cleanly. They fail because one component gets
**slow**, and the slowness spreads. This article explains how that cascade happens and the patterns
engineers use to stop it.

## Anatomy of a cascading failure

Your API calls a recommendations service on every product page. One day its database has a bad
moment and its responses take 20 seconds instead of 50 ms.

```text
1. Product page requests wait 20 s on recommendations.
2. Your API's worker threads / connections are all busy waiting.
3. New requests — even for pages that don't need recommendations — queue up.
4. Health checks time out; the load balancer marks instances unhealthy and removes them.
5. Remaining instances get even more traffic. Clients retry. Load doubles.
6. Everything is down, because a non-essential widget was slow.
```

Every pattern below breaks one link in this chain.

## 1. Timeouts (always first)

A call without a timeout ties up resources for as long as the dependency wants. Set timeouts on every
network call, based on the dependency's normal latency, inside an overall request deadline. See
[retries, timeouts and idempotency](/posts/retries-timeouts-and-idempotency).

## 2. Circuit breakers

If a dependency is failing, calling it again and waiting for the timeout every time wastes resources
and adds load to something already struggling. A **circuit breaker** wraps calls and tracks failures:

```text
         failures exceed threshold
 CLOSED ---------------------------> OPEN  (fail immediately, don't call)
   ^                                   |
   |  trial calls succeed              | after a cool-down period
   +---------- HALF-OPEN <-------------+
               (let a few trial calls through)
               trial calls fail -> back to OPEN
```

- **Closed:** calls pass through; failures (errors, timeouts) are counted over a sliding window.
- **Open:** calls fail **instantly** without touching the dependency — returning an error or a
  fallback in microseconds instead of waiting 20 seconds.
- **Half-open:** after a cool-down, a few trial requests test whether the dependency has recovered.

Typical settings: open when more than 50% of at least 20 calls in the last 10 seconds failed; cool
down for 30 seconds. Libraries: Resilience4j (Java), Polly (.NET), gobreaker (Go), opossum (Node.js);
service meshes like Istio/Envoy provide it as configuration ("outlier detection").

## 3. Fallbacks and graceful degradation

When a call fails or the circuit is open, what do you show? Decide per dependency:

| Dependency | Fallback |
|---|---|
| Recommendations | Hide the widget, or show popular items from a cache |
| Reviews | Show the page without reviews |
| Search | A simpler database query, or "search is temporarily unavailable" |
| Payments | No fallback — fail clearly, never pretend it worked |

Classify features as **critical** (checkout, login) and **nice-to-have** (recommendations, badges,
avatars). Nice-to-have features must never be able to break critical ones.

## 4. Bulkheads

Ships are divided into watertight compartments so a hull breach floods one compartment, not the whole
ship. In software: **isolate resources per dependency or per workload** so one can't exhaust what
others need.

- Separate connection pools / thread pools / concurrency limits per downstream service. If
  recommendations can use at most 20 concurrent calls, the other 180 workers stay available.
- Separate instance groups for different traffic: API vs admin vs batch jobs; free tier vs paying
  customers.
- Separate queues per job type so a flood of low-priority jobs doesn't delay password-reset emails.

A simple implementation is a semaphore around calls to each dependency:

```go
var recsLimit = make(chan struct{}, 20) // at most 20 concurrent calls

func getRecommendations(ctx context.Context, id string) ([]Item, error) {
    select {
    case recsLimit <- struct{}{}:
        defer func() { <-recsLimit }()
        return recsClient.Get(ctx, id)
    default:
        return nil, ErrBulkheadFull // fail fast instead of queueing
    }
}
```

## 5. Load shedding

When the whole service is overloaded, it is better to **serve some requests well than all requests
badly**. A server that accepts everything ends up with huge queues where every request times out —
zero useful work at 100% CPU.

- Limit concurrency or queue length, and reject excess requests immediately with `503` (cheap) rather
  than letting them wait (expensive).
- Prefer shedding by priority: health checks and critical paths first, batch and background traffic
  last.
- Shed **old** requests: if a request has waited longer than the client's timeout, the client has
  already given up — don't process it.
- **Adaptive concurrency limits** (inspired by TCP congestion control) automatically find how much
  concurrency the server can handle by watching latency.

## 6. Backpressure

In pipelines (queues, streams, WebSocket fan-out), a fast producer and a slow consumer lead to
unbounded buffers and eventually out-of-memory crashes. **Backpressure** pushes the slowness back to
the producer: bounded queues that block or reject when full, consumers pulling work at their own pace
(as Kafka consumers do), TCP flow control. Every queue in your system should have a maximum size and a
documented behaviour when it's full.

## 7. Retry budgets and jitter

Retries are load. During an outage, naive retries multiply traffic exactly when the system is
weakest. Use exponential backoff with jitter, retry at one layer only, and cap retries to a small
percentage of requests (a retry budget).

## 8. Make dependencies optional at startup

If your service refuses to start when a non-critical dependency is down, a full restart after an
outage becomes impossible. Start, report "degraded", and keep retrying in the background.

## Testing resilience

Patterns you haven't tested don't work. Ways to test:

- Unit/integration tests that inject latency and errors into clients.
- Fault injection in staging (Toxiproxy, service-mesh fault injection).
- **Game days**: deliberately break a dependency in a controlled way and watch what happens.
- **Chaos engineering**: continuously injecting failures in production, as Netflix popularised with
  Chaos Monkey — only once the basics are solid.

## Summary

| Pattern | Protects against |
|---|---|
| Timeouts | Waiting forever on a slow dependency |
| Circuit breaker | Repeatedly calling something that is failing |
| Fallbacks | Non-essential features breaking essential ones |
| Bulkheads | One dependency exhausting shared resources |
| Load shedding | Overload turning into total failure |
| Backpressure | Unbounded queues and memory exhaustion |
| Retry budgets + jitter | Retries amplifying an outage |

## Further reading

- Michael Nygard, *Release It!* (book) — the origin of many of these pattern names
- Google SRE Book: [Addressing Cascading Failures](https://sre.google/sre-book/addressing-cascading-failures/) and [Handling Overload](https://sre.google/sre-book/handling-overload/)
- Martin Fowler: [CircuitBreaker](https://martinfowler.com/bliki/CircuitBreaker.html)
- AWS Builders' Library: [Using load shedding to avoid overload](https://aws.amazon.com/builders-library/using-load-shedding-to-avoid-overload/)
- [Principles of Chaos Engineering](https://principlesofchaos.org/)
