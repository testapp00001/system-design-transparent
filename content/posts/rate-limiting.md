+++
title = "Rate limiting: token buckets, sliding windows and limiting across many servers"
summary = "Why every public endpoint needs a limit, how the four classic algorithms work, how to share limits between instances with Redis, and how to tell clients to back off politely."
tags = ["reliability", "api-design", "scalability", "security"]
level = "intermediate"
date = 2026-10-02
+++

A rate limiter answers one question for every request: **is this client allowed to do this again,
right now?** It protects you from brute-force login attempts, scrapers, buggy clients stuck in retry
loops, one customer's batch job starving everyone else, and runaway costs on expensive endpoints.

## Decide what you are limiting

Before picking an algorithm, decide on:

- **The key** — who is being limited: user id, API key, IP address (or IPv6 /64), tenant, or
  combinations ("per user per endpoint").
- **The limit** — e.g. 100 requests per minute, 5 login attempts per 10 minutes, 3 votes per poll.
- **The scope** — per endpoint, per group of endpoints, or global.
- **What happens when exceeded** — reject with `429`, queue, degrade (serve cached data), or just log
  (useful when introducing a new limit).

## The four classic algorithms

### Fixed window counter

Count requests in windows aligned to the clock (12:00:00–12:00:59, 12:01:00–…), reset at each window
boundary.

```text
limit 100/min:   [12:00 window: 100 ok, rest rejected][12:01 window: counter resets]
```

- Simple: one counter per key (`INCR` + `EXPIRE` in Redis).
- **Burst problem:** a client can send 100 requests at 12:00:59 and 100 more at 12:01:00 — 200 in two
  seconds.

### Sliding window log

Store the timestamp of each request; count those within the last 60 seconds.

- Exact.
- Memory grows with the limit (store up to N timestamps per key). Fine for small limits like login
  attempts, expensive for 10,000/minute.

### Sliding window counter

Approximate the sliding window using the current and previous fixed windows, weighted by overlap:

```text
estimate = current_count + previous_count × (fraction of previous window still inside the last 60 s)
e.g. 25 s into the minute: current=30, previous=90  ->  30 + 90 × (35/60) ≈ 82.5
```

- Two counters per key, smooths out boundary bursts. A good default for APIs.

### Token bucket

Each key has a bucket holding up to **B** tokens, refilled at **R** tokens per second. Each request
takes a token; no token, no request.

```text
capacity B = 20, refill R = 5/s
idle client accumulates up to 20 tokens -> can burst 20 requests -> then sustained 5/s
```

- Allows controlled **bursts** (B) while enforcing an average rate (R) — matches how real clients behave
  (a page load fires 10 requests at once, then nothing).
- Store just two numbers per key: tokens and last refill time; refill lazily on each request.
- Used widely, e.g. by cloud providers' API limits and many API gateways.

**Leaky bucket** is the mirror image: requests enter a queue that drains at a constant rate, smoothing
traffic for a downstream system that needs a steady pace.

## Limiting across many servers

With one instance, an in-memory map is enough (this site uses a simple in-memory fixed window for
login attempts — see `src/ratelimit.rs`). With N instances behind a load balancer, each instance only
sees part of a client's traffic, so per-instance limits let clients do N times more than intended.

Options:

1. **Central store (Redis).** All instances update the same counters. Make check-and-update
   **atomic**, or two instances can both read "99" and both allow request 100. In Redis, use `INCR`
   (atomic) for fixed windows, or a Lua script for token buckets so the read-modify-write happens in one
   step:

   ```lua
   -- KEYS[1] = bucket key; ARGV = capacity, refill_per_sec, now_ms
   local b = redis.call('HMGET', KEYS[1], 'tokens', 'ts')
   local cap, rate, now = tonumber(ARGV[1]), tonumber(ARGV[2]), tonumber(ARGV[3])
   local tokens = tonumber(b[1]) or cap
   local ts = tonumber(b[2]) or now
   tokens = math.min(cap, tokens + (now - ts) / 1000 * rate)
   local allowed = tokens >= 1
   if allowed then tokens = tokens - 1 end
   redis.call('HSET', KEYS[1], 'tokens', tokens, 'ts', now)
   redis.call('PEXPIRE', KEYS[1], math.ceil(cap / rate * 1000))
   return allowed and 1 or 0
   ```

2. **Database constraints.** For low-volume, business-level limits ("3 suggestions per poll"), the
   database is the natural place — with a lock or constraint so concurrent requests can't race past the
   limit. This site uses transaction-scoped advisory locks for its vote limits (`src/votes.rs`).
3. **Approximate local limits.** Divide the limit by the number of instances. Cheap and good enough when
   exactness doesn't matter.
4. **At the edge.** API gateways, CDNs and reverse proxies (nginx `limit_req`, Envoy, cloud WAFs)
   can rate-limit before traffic reaches your app — the best place for crude IP-based abuse limits.

If the central store is down, decide deliberately: **fail open** (allow — availability first) or
**fail closed** (reject — protection first). Login protection usually fails closed; general API limits
usually fail open.

## Telling clients what happened

Respond with **`429 Too Many Requests`** and tell the client when to come back:

```http
HTTP/1.1 429 Too Many Requests
Retry-After: 30
RateLimit-Limit: 100
RateLimit-Remaining: 0
RateLimit-Reset: 30
```

`Retry-After` is standard. The `RateLimit-*` fields come from an IETF draft that many APIs already
follow (with variations such as `X-RateLimit-*`). Well-behaved clients use these with backoff and
jitter (see [retries](/posts/retries-timeouts-and-idempotency)).

## Rate limiting vs load shedding

Rate limiting is about **fairness per client**: each key gets its quota regardless of server load.
**Load shedding** is about **protecting the server**: when the system is overloaded, reject some
requests (preferably low-priority ones) no matter who sent them. You want both — see
[resilience patterns](/posts/resilience-patterns).

## Common mistakes

- Limiting only by IP: many users share an IP (offices, mobile carriers' NAT), and attackers rotate IPs.
  Combine IP limits with account-level limits.
- Treating IPv6 addresses individually — one user may control a whole /64.
- Trusting `X-Forwarded-For` blindly, letting attackers pick their own "IP".
- No limit at all on login, password reset, signup and other expensive or abusable endpoints.
- Non-atomic check-then-increment in a shared store.

## Further reading

- Cloudflare blog: [How we built rate limiting capable of scaling to millions of domains](https://blog.cloudflare.com/counting-things-a-lot-of-different-things/)
- Stripe: [Scaling your API with rate limiters](https://stripe.com/blog/rate-limiters)
- IETF draft: [RateLimit header fields for HTTP](https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/)
- Redis docs: [INCR — pattern: rate limiter](https://redis.io/docs/latest/commands/incr/)
