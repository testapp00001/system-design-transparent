+++
title = "Observability: logs, metrics, traces, and SLOs that tell you what matters"
summary = "How to know what production is doing: structured logs, the RED and USE metrics, distributed tracing with OpenTelemetry, SLIs/SLOs/error budgets, and alerts that wake you up only when users are hurting."
tags = ["observability", "reliability", "devops"]
level = "intermediate"
date = 2026-10-02
+++

Something is wrong: users say the site is slow. Is it the database? A third-party API? One bad
instance? A specific endpoint? Without observability you guess, restart things and hope. With it, you
look at a dashboard, follow a trace, and know in minutes. This article explains the three main
signals and how to turn them into alerts that matter.

## Monitoring vs observability

- **Monitoring** answers questions you knew to ask in advance: "is CPU above 90%?", "is the error rate
  above 1%?".
- **Observability** is being able to answer *new* questions about production without shipping new
  code: "why are requests from mobile clients in Brazil slow since 14:05, only on the checkout
  endpoint?". It comes from rich, correlated telemetry.

You need both. The raw materials are logs, metrics and traces.

## Logs: what happened

A log line is a record of a discrete event. Make them **structured** (JSON or key=value), not
free-form sentences, so you can filter and aggregate:

```json
{"ts":"2026-10-02T09:14:03.120Z","level":"error","msg":"payment failed",
 "request_id":"req_8f2c1a","trace_id":"4bf92f3577b34da6","user_id":"usr_42",
 "order_id":"ord_1042","provider":"stripe","error_code":"card_declined","duration_ms":812}
```

Guidelines:

- Include a **request id / trace id** in every line so you can collect everything about one request.
- Log **events and decisions** (payment failed, retry scheduled, job finished) with their context —
  not every function entry.
- Use levels consistently: `error` = someone may need to act, `warn` = unusual but handled, `info` =
  significant business events, `debug` off in production.
- **Never log secrets or sensitive personal data**: passwords, tokens, full card numbers, session
  cookies. Redact at the logger.
- Logs are the most expensive signal at scale; sample noisy successful events, keep all errors.

Typical stacks: Loki + Grafana, the ELK/OpenSearch stack, or a hosted service.

## Metrics: how much, how often, how fast

Metrics are **numbers aggregated over time**: counters (requests total), gauges (queue length,
memory), histograms (latency distribution). They are cheap to store and fast to query, which makes
them the basis for dashboards and alerts.

Two standard recipes for *what* to measure:

**RED** — for every service and endpoint (request-driven things):

- **R**ate: requests per second.
- **E**rrors: failed requests per second (or %).
- **D**uration: latency distribution — p50, p95, p99.

**USE** — for every resource (CPU, memory, disk, connection pools, queues):

- **U**tilisation: how busy (%).
- **S**aturation: how much work is waiting (queue length, pool waiters).
- **E**rrors: resource errors.

Google's SRE book calls latency, traffic, errors and saturation the **four golden signals**.

### Percentiles, not averages

If 99 requests take 50 ms and 1 takes 5 seconds, the average is ~100 ms — which describes nobody.
The **p99** (99th percentile) says "1% of requests take ≥ 5 s". And a user loading a page that makes 20
requests will very likely hit at least one slow one, so tail latency is what users feel. Use
histograms so you can compute percentiles across instances.

### Watch cardinality

Each unique combination of label values creates a separate time series. Labels like `endpoint`,
`status_code` and `region` are fine; **`user_id`, `request_id` or full URLs as labels will explode your
metrics bill and storage**. High-cardinality detail belongs in logs and traces.

Typical stack: Prometheus (or a compatible system) + Grafana, or a hosted equivalent.

## Traces: where the time went

In a system where one user request touches an API, three services, a database and a cache, a
**distributed trace** shows the whole journey as a tree of **spans**, each with a start time and
duration:

```text
POST /checkout                                   [=========================== 820 ms]
  auth.verify_session                            [= 12 ms]
  cart-service GET /cart/42                        [=== 45 ms]
    postgres SELECT cart_items                       [== 30 ms]
  payment-service POST /charges                          [=================== 690 ms]
    stripe POST /v1/payment_intents                        [================= 640 ms]   <- here
  orders INSERT                                                                  [= 25 ms]
```

The trace context (trace id + parent span id) is propagated between services in headers — the W3C
`traceparent` header is the standard. **OpenTelemetry** is the vendor-neutral standard for producing
traces (and metrics and logs) with SDKs and auto-instrumentation for most languages and frameworks;
backends include Jaeger, Grafana Tempo, Zipkin and many hosted products.

Traces are usually **sampled** (e.g. keep 10% of normal traces but all slow or failed ones —
"tail-based sampling") to control cost.

## Connecting the three

The real power comes from linking them: a latency **metric** spikes → you open example **traces** from
that period (exemplars) → the slow span links to its **logs** via the trace id. Put `trace_id` in your
log lines and you get this almost for free.

## SLIs, SLOs and error budgets

Dashboards full of graphs don't tell you whether things are *good enough*. SLOs do.

- **SLI (service level indicator)**: a measurement of what users experience. "Proportion of checkout
  requests that succeed in under 500 ms."
- **SLO (service level objective)**: the target for that SLI over a window. "99.9% over 30 days."
- **Error budget**: what the SLO allows to fail. 99.9% over 30 days ≈ **43 minutes** of total failure
  (or many more minutes of partial failure).

```text
SLO          allowed failure per 30 days
99%          ~7.2 hours
99.9%        ~43 minutes
99.95%       ~22 minutes
99.99%       ~4.3 minutes
```

The error budget turns reliability into a shared, numeric decision: while budget remains, ship
features fast; when it's burned, prioritise reliability work. And it reminds everyone that 100% is the
wrong target — each extra nine costs dramatically more, and users can't tell the difference beyond the
reliability of their own network and devices.

An **SLA** (agreement) is a contract with consequences (refunds) — set it looser than your internal SLO.

## Alerts that respect your sleep

- **Alert on symptoms users feel** (SLO burn: error rate, latency), not on every cause (CPU at 85%).
  Causes go on dashboards.
- **Burn-rate alerts**: page when the error budget is being consumed fast enough to be exhausted in
  hours (e.g. 14× the sustainable rate over the last hour); open a ticket for slow burns.
- Every page must be **actionable** and link to a runbook. If the response to an alert is "ignore it",
  delete the alert.
- Watch for **silence** too: a job that stopped running, a queue nobody consumes (absence alerts,
  dead man's switches).

## Getting started on a small app

1. Structured logs with a request id on every line.
2. RED metrics per endpoint (most frameworks have middleware) + resource metrics for DB connections
   and queues.
3. A health check endpoint (this site exposes `/healthz`, which also checks the database).
4. One dashboard per service: rate, errors, p50/p95/p99 latency, saturation.
5. OpenTelemetry tracing once you have more than one service or significant external calls.
6. One SLO for your most important user journey, with a burn-rate alert.

## Further reading

- Google SRE Book: [Monitoring Distributed Systems](https://sre.google/sre-book/monitoring-distributed-systems/) and [Service Level Objectives](https://sre.google/sre-book/service-level-objectives/)
- Google SRE Workbook: [Alerting on SLOs](https://sre.google/workbook/alerting-on-slos/)
- [OpenTelemetry documentation](https://opentelemetry.io/docs/)
- Brendan Gregg: [The USE Method](https://www.brendangregg.com/usemethod.html)
- Tom Wilkie: The RED Method (Grafana Labs blog and talks)
