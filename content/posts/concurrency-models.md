+++
title = "Concurrency models: threads, async/await, event loops and actors"
summary = "How servers handle many requests at once: processes and threads, event loops, async/await, green threads and actors. What each model costs, why blocking an event loop stalls everyone, and how to choose for a web backend."
tags = ["backend","performance"]
level = "intermediate"
date = 2026-10-02
+++

Your Node.js API answers in 20 ms. Then someone adds an endpoint that builds a large CSV report in
memory. Whenever one user clicks "export", *every* other request on that server waits too, health
checks time out, and the load balancer marks the instance as unhealthy. The CSV code is not wrong.
The surprise comes from the **concurrency model**: the way your runtime runs many requests at the
same time. In a classic Java server, the same code would block only one thread.

## The problem: most of a request is waiting

- **Concurrency** means many tasks are *in progress* at the same time, perhaps taking turns on one core.
- **Parallelism** means many tasks *run* at the same instant, on different cores.

A typical web request computes a little and waits a lot: for the database, another HTTP API, a
cache. Work that mostly waits for the network or disk is **I/O-bound**. Work that keeps a CPU core
busy (resizing images, hashing passwords, compressing data) is **CPU-bound**.

Little's law says: *requests in flight = arrival rate × time per request*. At 1,000 requests per
second and 200 ms each, about 200 requests are in flight at any moment (see
[queueing basics](/posts/queueing-theory-littles-law)). Most of them are just waiting. So the key
question for any model is: **what does a waiting request cost, and what does the CPU do meanwhile?**

## The building blocks: processes and threads

A **process** is a running program with its own private memory. The operating system (OS) isolates
processes: if one crashes, the others continue.

A **thread** runs inside a process. Threads of one process share memory: sharing data is cheap, and
so is corrupting it by accident. Each thread has its own **stack** (memory for local variables and
function calls). The runtime or the OS usually reserves between 1 MB and 8 MB of virtual memory for
each stack (Java's default is about 1 MB on 64-bit Linux). Real memory is used only as the stack
grows, but each thread still costs memory and scheduling work.

The OS **scheduler** decides which thread runs on which core. It is **preemptive**: it can pause a
thread at any moment to run another. Each switch (a **context switch**) costs some CPU time.

## Model 1: one process or thread per request

The oldest model gives each request its own worker. The worker **blocks** (stops and waits) on every
I/O call, and meanwhile the OS runs other workers.

```text
                     server (pool of 4 workers)
 request A ---> [worker 1]  parse ... wait for DB ........ render ... reply
 request B ---> [worker 2]  parse ... wait for payment API ............ reply
 request C ---> [worker 3]  parse ... wait for DB ... reply
 request D ---> [worker 4]  parse ... wait for cache ... reply
 request E ---> (all workers busy) --> waits in a queue, or gets an error
```

- **PHP-FPM** keeps a pool of worker *processes*, each handling one request at a time
  (`pm.max_children` sets the maximum).
- **Classic Java servlet containers** such as Tomcat use a pool of threads, one per request in
  progress (Tomcat's default maximum, the `maxThreads` setting, is 200).
- **Rails** apps usually run on Puma, often as a few processes with a few threads each. Python's
  Gunicorn uses synchronous worker processes by default (one request at a time per process).

The good parts: code is plain and sequential, and a slow request blocks only its own worker. The
costs: every waiting request holds a whole thread or process. Thousands are fine; tens of thousands
of idle connections (WebSockets, long polling) waste memory. And when one dependency gets slow, all
workers end up waiting on it, and the server stops answering even requests that never touch it.
Timeouts are the main defence (see [retries and timeouts](/posts/retries-timeouts-and-idempotency)).

> [!NOTE]
> CRuby and CPython have a global lock (the GVL in Ruby, the GIL in Python): only one thread runs
> interpreter code at a time. It is released during I/O waits, so threads still help there, but
> CPU-bound work needs *processes*. CPython 3.13 added an experimental "free-threaded" build without
> the GIL (PEP 703), and Python 3.14 made it officially supported. It is still a separate, optional
> build, not the default, and some libraries do not support it yet.

## Model 2: the event loop

To hold more connections than threads handle cheaply (the "C10K problem": 10,000 clients at once),
servers adopted the **event loop**: one thread that never waits for any single connection.

The OS can watch thousands of sockets and report which ones are ready (`epoll` on Linux, `kqueue` on
BSD and macOS; Windows uses IOCP, which reports finished operations instead). The loop asks "what is
ready?", runs the code for each ready event, and asks again. A waiting request costs only a small
object and a callback. Node.js works this way. nginx runs one event loop in each of a few worker
processes, and Redis runs all commands on one main thread with an event loop.

```text
   +----------------------------------------------------------------------+
   |  event loop: ONE thread runs all of your JavaScript                  |
   |  forever: ask "what is ready?", run each ready callback              |
   +----------------------------------------------------------------------+
          ^                                         ^
          | "socket 17 has data"                    | "file read finished"
   +-------------------------+         +----------------------------------+
   | OS kernel: epoll/kqueue |         | libuv thread pool (default: 4)   |
   | watches every socket    |         | files, dns.lookup, crypto, zlib  |
   +-------------------------+         +----------------------------------+
```

In Node.js, network I/O needs no extra threads. Work that blocks at the OS level (file system calls,
`dns.lookup`) or is CPU-heavy (async `crypto.pbkdf2`, async `zlib` calls) goes to the **libuv thread
pool**, which has 4 threads unless you set the `UV_THREADPOOL_SIZE` environment variable at startup.
All these tasks share this small pool, so many slow file or DNS calls can queue behind each other.

### Why blocking the loop stalls everyone

The loop runs one callback at a time and cannot interrupt it. If one callback computes for 200 ms,
**nothing else in that process happens for 200 ms**: no new requests, no timers, no health checks.

```js
const crypto = require("node:crypto");
const { promisify } = require("node:util");
const pbkdf2 = promisify(crypto.pbkdf2);

// BAD: hashing runs on the one JavaScript thread, so everyone waits
app.post("/login", (req, res) => {
  const hash = crypto.pbkdf2Sync(req.body.password, salt, ITERATIONS, 64, "sha512");
  // ... compare the hash and send the response
});

// GOOD: hashing runs on the libuv thread pool; the loop keeps serving others
app.post("/login", async (req, res) => {
  const hash = await pbkdf2(req.body.password, salt, ITERATIONS, 64, "sha512");
  // ... compare the hash and send the response
});
```

Other blockers: `JSON.parse` on a huge body, a slow regular expression, `fs.readFileSync`. Move
CPU-heavy work to `worker_threads`, another process, or a
[background job](/posts/background-jobs-and-cron).

## Model 3: async/await and futures

**async/await** lets you write event-loop code that *looks* sequential. A **future** (a *promise* in
JavaScript, a *Task* in C#) is a value that will be ready later. `await` means: "pause this function,
let other tasks run, continue when the value is ready." Only the function pauses, not the thread.

The idea is the same everywhere, but runtimes differ:

- **Python asyncio** uses one thread and one event loop, like Node.js. One blocking call stops
  everything.
- **Rust's Tokio** runs, by default, one worker thread per CPU core; idle workers "steal" tasks from
  busy ones. Blocking a worker still delays every task queued on it, so Tokio offers `spawn_blocking`.
- **C# / .NET** runs tasks on a shared thread pool. The classic trap is calling `.Result` or
  `.Wait()` on a task: it blocks a pool thread while it waits, which can starve the pool under load.
  In UI apps and the older ASP.NET (not ASP.NET Core), it can even deadlock.

The most common bug looks innocent:

```python
# BAD: 'requests' is synchronous. The whole event loop stops until the response arrives.
async def get_rates():
    return requests.get(RATES_URL, timeout=10).json()

# GOOD: an async client gives control back to the loop while it waits.
async def get_rates(client: httpx.AsyncClient):
    resp = await client.get(RATES_URL, timeout=10)
    return resp.json()
```

No async version of a library? `asyncio.to_thread()` (Python 3.9+) runs a blocking call in a
separate thread, so the loop keeps going. This helps for blocking I/O; for CPU-heavy work the GIL
still applies, so use a process pool instead.

Async/await has two costs. It is **contagious**: a function that awaits must itself be `async`, so
async spreads up the call stack and you need async database drivers and HTTP clients. And scheduling
is **cooperative**: a task gives up the CPU only at an `await`.

## Model 4: green threads (goroutines and virtual threads)

What if blocking code were cheap? **Green threads** are managed by the language runtime, not the OS.
The runtime runs many of them on a few OS threads (an **M:N** model). When a green thread waits for a
socket, the runtime parks it and runs another one. You write normal blocking code and get event-loop
efficiency.

```text
   green threads:   g1  g2  g3  g4  g5  g6  g7  g8  g9 ... (thousands, cheap)
                      \  |  /     \  |  /     \  |  /
   runtime scheduler:  parks a waiting green thread, runs a ready one
                         |           |           |
   OS threads:      [thread 1]  [thread 2]  [thread 3]     (about one per core)
```

- **Go** was built on this idea. Goroutines start with a stack of a few kilobytes that grows as
  needed. They run on `GOMAXPROCS` OS threads at a time (by default, the number of CPU cores). The
  standard `net/http` server starts a goroutine for each connection.
- **Java virtual threads** became final in JDK 21 (JEP 444). Existing blocking code (JDBC, servlets,
  HTTP clients) runs on them with little or no change:

```java
try (var executor = Executors.newVirtualThreadPerTaskExecutor()) {
    for (Order order : orders) {
        executor.submit(() -> shippingApi.quote(order)); // a plain blocking call
    }
} // close() waits for all tasks to finish
```

Green threads make *waiting* cheap, not *work*. They do not speed up CPU-bound code, and if 10,000
virtual threads need a database connection from a pool of 20, then 9,980 wait (see
[connection pooling](/posts/connection-pooling)). In JDK 21, a virtual thread that blocks inside a
`synchronized` block also *pins* its OS thread: the OS thread stays busy and cannot run other virtual
threads. JDK 24 removed most of this limit (JEP 491), but pinning still happens in some cases, such as
calls into native code. Don't pool virtual threads: create one per task, and limit access to scarce
resources with a semaphore or a connection pool instead.

## Model 5: actors

An **actor** has private state, a **mailbox** (a queue of incoming messages), and code that handles one
message at a time. Actors never share memory; to change another actor's state, you send it a message.
Because messages are handled one by one, the state needs no locks.

```text
  [actor: room 42]                         [actor: user 7]
   state:   members, last 50 messages       state:   open connections
   mailbox: [join] [msg] [msg] [leave]      mailbox: [deliver]
        |                                        ^
        +-------- send {deliver, msg} -----------+
```

- **Erlang and Elixir** run on the **BEAM** virtual machine. Their "processes" are actors: very light,
  each with its own heap and garbage collection, scheduled *preemptively* so one busy process cannot
  freeze the others. **Supervisors** restart crashed processes ("let it crash").
- **Akka** and **Apache Pekko** bring actors to the JVM. Pekko is an open-source fork of Akka 2.6,
  made after Akka changed its license. **Microsoft Orleans** offers "virtual actors" for .NET.

Actors fit many long-lived things with state: a chat room, a game match, a device (see
[scaling a WebSocket chat](/posts/scaling-websockets-chat)). The costs: mailboxes grow without limit
unless you add backpressure, and a bug spread across many messages is harder to follow than one stack
trace.

## Shared memory vs message passing

Threads usually **share memory** and protect it with locks. Actors and Go channels prefer **message
passing**: send a copy of the data, or hand over ownership. As the Go guide *Effective Go* puts it:
"Do not communicate by sharing memory; instead, share memory by communicating."

A **race condition** happens when the result depends on timing. `count += 1` is really "read, add,
write", so two threads can both read 5 and both write 6. Single-threaded async code is *not* safe
either, because other tasks run at every `await`:

```python
async def withdraw(account_id, amount):
    balance = await get_balance(account_id)   # requests 1 and 2 both read 100
    if balance >= amount:                      # both see 100 >= 80
        await set_balance(account_id, balance - amount)  # both write 20; 160 paid out
```

In a web backend, shared state usually lives in the database, so fix it there with one atomic
statement:

```sql
UPDATE accounts SET balance = balance - 80
WHERE id = 42 AND balance >= 80;   -- 0 rows updated means "not enough money"
```

See [transactions and isolation levels](/posts/transactions-and-isolation-levels), and
[distributed locks](/posts/distributed-locks) when many servers must coordinate.

A **deadlock** happens when tasks wait for each other forever: thread 1 holds lock A and waits for B,
thread 2 holds B and waits for A. Two actors waiting for replies from each other deadlock too, and so
does a pool where every request holds one connection and waits for a second. Defences: take locks in
the same order everywhere, hold them briefly, and put a timeout on every wait.

## CPU-bound vs I/O-bound: where each model shines

| Model | I/O-bound work | CPU-bound work | Examples | Main risk |
|---|---|---|---|---|
| Thread/process per request | Good up to thousands | Good (processes in Ruby/Python) | PHP-FPM, Tomcat, Puma | Pool runs dry |
| Event loop | Excellent | Poor: blocks everyone | Node.js, nginx, Redis | One slow callback stalls all |
| async/await | Excellent | Poor unless offloaded | asyncio, Tokio, .NET | Hidden blocking calls |
| Green threads | Excellent, blocking-style code | OK: uses all cores, but no faster than OS threads | Go, Java virtual threads | Unbounded concurrency |
| Actors | Excellent for stateful sessions | OK; the BEAM stays fair, but is slow at heavy math | Erlang/Elixir, Akka, Orleans | Growing mailboxes |

## How to choose for a web backend

Usually your language has already chosen for you; use its mainstream model well. Within that:

1. **A typical CRUD API with a database:** any model works, including thread-per-request. The
   database is usually the bottleneck, not your threads.
2. **Many long-lived, mostly idle connections** (WebSockets, SSE): use an event loop, async/await,
   green threads or the BEAM, not one OS thread per connection. See
   [polling, SSE and WebSockets](/posts/realtime-polling-sse-websockets).
3. **Calling many slow APIs in parallel:** async/await or green threads make this easy.
4. **CPU-heavy work** (images, PDFs, big reports): keep it off the request path, in a background job
   or a worker pool sized to the number of cores.
5. **Many small stateful entities that message each other** (rooms, matches, devices): consider actors.

> [!TIP]
> Whatever the model, **bound concurrency at every layer**: a database pool size, a semaphore for
> outbound calls, a maximum queue length. Cheap tasks are easy to start by the 100,000; your
> dependencies cannot take that.

## Common mistakes

- **Blocking inside async code:** `requests` or `time.sleep` in asyncio, `fs.readFileSync` in a Node.js
  handler, `std::thread::sleep` in Tokio, `.Result` in C#. To find them: Python's asyncio debug mode
  (`PYTHONASYNCIODEBUG=1`) logs callbacks slower than 100 ms by default; Node.js offers
  `perf_hooks.monitorEventLoopDelay()` to measure how late the loop runs.
- **Unbounded fan-out:** `Promise.all` over 50,000 URLs, or one goroutine per row of a huge table. Use a
  limit, such as a semaphore or Go's `errgroup` with `SetLimit`.
- **Ignoring container CPU limits.** Pools are often sized from the host's CPU count, not the
  container's limit. Check how your runtime counts CPUs (Go's default became container-aware on Linux
  in Go 1.25).
- **Adding workers "for safety".** Past what your CPU cores (for CPU-bound work) or the database can
  handle, more workers add contention, not throughput. Measure saturation (loop delay, busy workers, pool wait time) and
  [profile](/posts/profiling-and-performance-debugging).

## Further reading

- Node.js: [Don't Block the Event Loop (or the Worker Pool)](https://nodejs.org/en/learn/asynchronous-work/dont-block-the-event-loop)
- Python documentation: [Developing with asyncio](https://docs.python.org/3/library/asyncio-dev.html)
- Tokio: [Tutorial](https://tokio.rs/tokio/tutorial)
- OpenJDK: [JEP 444: Virtual Threads](https://openjdk.org/jeps/444)
- Go blog: [Concurrency is not parallelism](https://go.dev/blog/waza-talk) (video of Rob Pike's talk)
- Fred Hébert: [Learn You Some Erlang for Great Good!](https://learnyousomeerlang.com/)
