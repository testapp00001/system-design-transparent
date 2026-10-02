+++
title = "Profiling and performance debugging: finding where the time really goes"
summary = "A step-by-step method for slow systems: break latency down with traces, check resources with the USE method, read flame graphs, hunt memory leaks, find slow queries, load test without fooling yourself, and recognise the usual culprits."
tags = ["performance","observability","backend"]
level = "intermediate"
date = 2026-10-02
+++

The dashboard says `GET /orders` got slow: its p99 (the time that 99% of requests stay under) went
from 300 ms to 2 seconds. One person wants to add a cache; another blames the database. Both are
guessing. Performance work most often goes wrong right here: people change code before they know
where the time goes. This article gives you a repeatable method, and the tools to follow it.

## Measure before you optimise

The slow part is rarely the code that *looks* slow. So start with a measurable problem statement:

- **What** is slow: one endpoint, one job, the whole service?
- **How** slow, as a percentile: "p99 is 2 s; the target is 500 ms".
- **Since when**: a deploy, more traffic, more data ("our biggest customer now has 200,000 orders")?

Then remember simple arithmetic. If a step takes 5% of the request time, making it ten times faster
saves less than 5%. Fix the biggest part first, change **one thing at a time**, and measure again
the same way.

## Step 1: break the latency down with traces

A **distributed trace** records one request as a tree of **spans**: timed steps such as "SQL query"
or "HTTP call to the payment service". If you have tracing (see
[observability: logs, metrics and traces](/posts/observability-logs-metrics-traces)), open a few
*slow* traces, not average ones, and ask: where is the time?

```text
 "p99 of GET /orders went from 300 ms to 2 s"
     |
     v
 Open traces of slow requests. Where is the time?
     |
     +--> in database spans ............ pg_stat_statements, EXPLAIN, lock waits
     |
     +--> in calls to other services ... follow the trace into that service
     |
     +--> in your own code (CPU) ....... CPU profiler, flame graph
     |
     +--> in gaps between spans ........ waiting: for a thread, a pool connection, a lock, GC
                                         -> USE method, off-CPU and block profiles
```

Three shapes appear again and again:

- **Many small, identical spans** (50 times `SELECT ... FROM customers WHERE id = $1`): an N+1
  query problem. Each query is fast; there are just too many.
- **One long span**: a slow query or a slow dependency.
- **Gaps** with no span: the code was waiting for something nobody instrumented. That is usually a
  queue: a thread pool, a [connection pool](/posts/connection-pooling), a lock, or a garbage
  collection pause.

## Step 2: check the machine with the USE method

Sometimes no request does anything wrong; the server is simply overloaded. Brendan Gregg's **USE
method** is a checklist for that. For every resource, check:

- **Utilisation**: how busy is it?
- **Saturation**: how much work is *waiting* for it (a queue)?
- **Errors**: is it failing?

| Resource | Utilisation | Saturation | Errors |
|---|---|---|---|
| CPU | `mpstat -P ALL 1`, `top` | `vmstat 1`: `r` column above the CPU count | rare |
| Memory | `free -m` | swapping (`vmstat` `si`/`so`), OOM kills in `dmesg` | failed allocations |
| Disk | `iostat -xz 1`: `%util` | `iostat`: queue size, wait time | I/O errors in `dmesg` |
| Network | `sar -n DEV 1` vs link speed | drops, TCP retransmits | interface errors |
| Connection pool | connections in use / max | requests waiting for a connection | acquire timeouts |

Saturation matters most: a resource that is 90% busy makes requests wait far longer than one at
70%, as [queueing theory](/posts/queueing-theory-littles-law) explains.

## Step 3: CPU profiling and flame graphs

If the time is in your own code, you need a **profiler**. There are two kinds:

- **Instrumenting profilers** record every function call. Counts are exact, but the large overhead
  distorts the timing.
- **Sampling profilers** interrupt the program many times per second (for example 99 times) and
  record the current **call stack**: the running function and the functions that called it. A
  function present in 30% of samples used roughly 30% of the CPU time. The overhead is low, so many
  are safe in production.

Thousands of stacks are hard to read as text. Brendan Gregg invented the **flame graph** to show
them in one picture:

```text
        +-----------------------------------+
        | json.Marshal (45%)                |
+-------+-----------------------------------+-------+
| db 10%| buildResponse (55%)                       |
+-------+-------------------------------------------+---------------------------+
| handleOrders (65%)                                | runtime GC (35%)          |
+---------------------------------------------------+---------------------------+
| all samples (100%)                                                            |
+-------------------------------------------------------------------------------+
```

How to read it:

- Each box is a function. The box above it is a function it called.
- **Width** is the share of samples containing that function. Wide means expensive.
- Left-to-right order **is not time** (Chrome DevTools' *flame chart* is different: there the
  horizontal axis is time).
- Look for **wide boxes near the top**: functions using CPU themselves. Here JSON encoding takes
  45% of the CPU, and the garbage collector another 35%, probably because encoding allocates a lot.

> [!WARNING]
> A CPU profile only shows time spent **on the CPU**. A request waiting 800 ms for the database or a
> lock uses almost no CPU, so it is nearly invisible in a CPU flame graph. For waiting, use traces,
> **wall-clock** profiling (async-profiler's `wall` mode, py-spy's `--idle` option), Go's block and
> mutex profiles, or Gregg's *off-CPU* flame graphs.

## Profilers by ecosystem

| Ecosystem | CPU | Memory | Notes |
|---|---|---|---|
| Linux, native code | `perf` + FlameGraph scripts | `heaptrack` | Needs symbols; JIT runtimes need extra setup |
| Go | `net/http/pprof`, `go tool pprof` | heap profile | Also goroutine, block, mutex profiles |
| JVM | async-profiler, JDK Flight Recorder | async-profiler `alloc` mode, heap dumps | async-profiler avoids safepoint bias |
| Python | py-spy | `tracemalloc`, memray | py-spy attaches to a running process |
| Rust | `cargo flamegraph` | `heaptrack` | Keep debug symbols in release builds |
| Node.js | `--cpu-prof`, Chrome DevTools, Clinic.js | heap snapshots | |

Some starting commands:

```sh
# Linux perf: sample one process at 99 Hz for 30 s, then draw a flame graph
perf record -F 99 -g -p <pid> -- sleep 30
perf script | ./stackcollapse-perf.pl | ./flamegraph.pl > cpu.svg

py-spy record -o cpu.svg --pid <pid>   # Python: no code change, no restart
py-spy dump --pid <pid>                # what is every thread doing right now?
asprof -d 30 -f cpu.html <pid>         # JVM, async-profiler 3.x
cargo flamegraph --bin my-server       # Rust
node --cpu-prof server.js              # Node.js: .cpuprofile on exit, open in DevTools
```

In Go, the profiler is in the standard library. Serve it on a **separate, internal-only** port:

```go
import (
    "net/http"
    _ "net/http/pprof" // registers the /debug/pprof/ handlers
)

func main() {
    go http.ListenAndServe("localhost:6060", nil) // never expose to the internet
    // ... start the real server on another port
}
```

Then run `go tool pprof -http=:8081 'http://localhost:6060/debug/pprof/profile?seconds=30'` and
open the flame graph view.

> [!NOTE]
> Profilers in containers often need extra permissions (for py-spy in Docker, the `SYS_PTRACE`
> capability). Sort this out before an incident.

## Memory: allocation pressure and leaks

**Allocation pressure.** In garbage-collected languages (Go, Java, C#, JavaScript, Python), every
allocation is future work for the garbage collector (GC): more CPU and, depending on the runtime,
more or longer pauses. An **allocation profile** (Go `alloc_space`, async-profiler `alloc` mode)
shows who allocates most. Then allocate less: reuse buffers, return less data.

**Leaks.** In a GC language, a leak means objects you no longer need are still *referenced*, so the
GC cannot free them. Memory grows until the process is killed. To hunt one:

1. **Confirm the pattern.** A sawtooth returning to the same baseline is normal; a baseline that
   keeps rising is a leak. Watch heap metrics, not only process size (RSS): many runtimes keep freed
   memory instead of returning it to the operating system.
2. **Take two heap snapshots** some time apart, under load, and **compare** them. Go:
   `go tool pprof -base heap1.pb.gz heap2.pb.gz`. Python: `tracemalloc` snapshots and
   `compare_to()`. JVM: `jcmd <pid> GC.heap_dump heap.hprof`, opened in Eclipse MAT. Node.js:
   heap snapshots in Chrome DevTools.
3. **Ask what keeps the growing objects alive.** Heap tools show the chain of references
   ("retainers") that holds each object.

Usual causes: a map used as a cache with no size limit, listeners added but never removed,
goroutines or threads blocked forever, and global lists that only grow.

> [!WARNING]
> A heap dump contains everything in memory: passwords, tokens, personal data. Treat it like a
> database backup. Taking one can pause the process, so first remove the instance from the load
> balancer.

## The database side

When the trace points at the database, ask it which queries cost the most. In PostgreSQL, enable
the `pg_stat_statements` extension (it must be in `shared_preload_libraries`, which needs a
restart) and sort by **total** time:

```sql
SELECT calls,
       round(total_exec_time)              AS total_ms,
       round(mean_exec_time::numeric, 2)   AS mean_ms,
       left(query, 80)                     AS query
FROM pg_stat_statements
ORDER BY total_exec_time DESC
LIMIT 10;
```

A 2 ms query called 50,000 times per minute costs far more than a 2-second report run once an hour.
(These column names are from PostgreSQL 13 and later.) PostgreSQL can also log statements slower
than a threshold (`log_min_duration_statement`), and `auto_explain` logs their plans.

In MySQL, turn on the **slow query log** (`slow_query_log = ON`) and lower `long_query_time`
(in seconds; the default is 10). Summarise the log with `mysqldumpslow` or Percona's
`pt-query-digest`.

Then run `EXPLAIN ANALYZE` on realistic data. If the query reads far more rows than it returns, an
index is usually missing or unusable; see
[database indexes and reading EXPLAIN](/posts/database-indexes-and-explain). If a query is fast
alone but slow in production, check **lock waits**: in PostgreSQL, the `wait_event_type` column of
`pg_stat_activity` shows what each session is waiting for.

## Load testing without fooling yourself

Some problems only appear under load. A **load test** sends traffic at a controlled rate to find
limits before users do. Realism matters more than the tool:

- **Production-sized data.** A database with 100 rows hides every missing index.
- **A realistic mix** of endpoints and **many different ids**, not one id the cache always answers.
- **Minutes, not seconds**, to see GC cycles, cache eviction and pool exhaustion.
- **A load generator that is not the bottleneck.** Watch its CPU too.
- **Never against third parties** you do not control, such as a real payment provider.

| Tool | Scripts | How it sends load |
|---|---|---|
| k6 | JavaScript | Fixed number of virtual users, or a fixed arrival rate |
| wrk | Lua (optional) | Fixed number of connections, as fast as possible |
| wrk2 | Lua (optional) | Constant request rate (`-R`) |
| vegeta | Command line or Go library | Constant request rate |

### Coordinated omission

Many load tools are **closed-loop**: each connection sends a request, waits for the response, then
sends the next. This hides the worst latency. Gil Tene named the problem **coordinated omission**:
the tool slows down together with the server and *omits* the requests it never sent.

An example. A test runs for 60 seconds on one connection. The server answers in 1 ms but freezes
once for 5 seconds. The tool records tens of thousands of 1 ms results and **one** 5-second result,
so it reports a p99 of about 1 ms. Real users do not wait politely. If 500 users per second keep
arriving, about 2,500 of them arrive during the freeze and wait up to 5 seconds. That is more than
1% of all requests, so the true p99 is several seconds.

The fix is an **open model**: send requests at a fixed rate whether or not earlier ones finished,
and measure from when each request *should* have been sent. wrk2 and vegeta do this. In k6, use an
arrival-rate executor:

```js
import http from 'k6/http';

export const options = {
  scenarios: {
    orders: {
      executor: 'constant-arrival-rate',
      rate: 200,                          // 200 iterations per second
      timeUnit: '1s',
      duration: '5m',
      preAllocatedVUs: 100,
      maxVUs: 500,
    },
  },
  thresholds: { http_req_duration: ['p(99)<500'] }, // fail if p99 >= 500 ms
};

export default function () {
  const id = Math.floor(Math.random() * 100000) + 1;
  http.get(`https://staging.example.com/api/orders/${id}`);
}
```

Raise the rate step by step. Latency stays flat, then rises sharply as a resource saturates. That
"knee" is your real capacity.

## Continuous profiling in production

Many problems never reproduce in staging. **Continuous profiling** runs a low-rate sampling
profiler on production servers all the time and stores the profiles, like metrics. You can open a
flame graph for "the API servers yesterday at 14:00", **compare** before and after a deploy, and
find the most expensive functions across the fleet.

Open-source options include **Pyroscope** (now part of Grafana Labs) and **Parca**, which uses eBPF
to profile processes without code changes. Google described the idea at large scale in its paper
*Google-Wide Profiling*.

## The usual culprits

| Culprit | What you see | Typical fix |
|---|---|---|
| N+1 queries | Many identical small DB spans per request | Eager loading or batching ([ORMs and N+1](/posts/orm-n-plus-one)) |
| Missing index | High total time in `pg_stat_statements`; sequential scan in `EXPLAIN` | An index for the whole query |
| Lock contention | Low CPU, high latency, threads waiting | Shorter critical sections, less shared state |
| GC pressure | Wide GC boxes in the flame graph; latency spikes | Allocate less, then tune heap size |
| Chatty network calls | Long chains of sequential calls to other services | Batch, call in parallel, cache |
| Serialisation | JSON encoding or decoding wide in the flame graph | Return fewer fields, encode once |
| Pool exhaustion | Gaps in traces; waiting for a connection | Fix slow queries first, then size the pool |

Many of these are about **waiting**, not computing. That is why traces come before CPU profiles.

## Trade-offs: when to stop

- **Good enough is a number.** When the endpoint meets its target (its SLO), stop. More tuning
  usually makes code harder to read for little gain.
- **Sometimes the fix is design, not code.** If a page needs 40 calls to other services, no
  micro-optimisation will save it.

## Common mistakes

- **Optimising without measuring**, or measuring averages instead of percentiles.
- **Reading a CPU flame graph for a waiting problem.** The slow part may not appear at all.
- **Profiling an unrealistic setup**: a debug build, a tiny dataset, or a cold JIT (the JVM and V8
  optimise code only after it has run for a while).
- **Trusting closed-loop load test latency**, or testing one hot key that the cache always answers.
- **Changing several things at once**, then not measuring again under the same conditions.

## Further reading

- Brendan Gregg: [Flame Graphs](https://www.brendangregg.com/flamegraphs.html) and [The USE Method](https://www.brendangregg.com/usemethod.html)
- Brendan Gregg: *Systems Performance: Enterprise and the Cloud* (book)
- Go documentation: [Diagnostics](https://go.dev/doc/diagnostics) and [net/http/pprof](https://pkg.go.dev/net/http/pprof)
- PostgreSQL documentation: [pg_stat_statements](https://www.postgresql.org/docs/current/pgstatstatements.html)
- Gil Tene: [wrk2](https://github.com/giltene/wrk2), whose README explains coordinated omission, and his talk *How NOT to Measure Latency*
