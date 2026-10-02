+++
title = "Caching strategies: cache-aside, write-through, invalidation and stampedes"
summary = "Where to cache, the main caching patterns, how to keep caches from serving wrong data, and how to stop a popular key expiring from taking down your database."
tags = ["caching", "performance", "scalability", "backend"]
level = "intermediate"
date = 2026-10-02
+++

A cache stores the result of expensive work so you don't redo it. Done well, it turns a 50 ms
database query into a 0.5 ms memory lookup and lets one database serve ten times the traffic. Done
badly, it serves stale prices, leaks one user's data to another, or collapses your database the moment
it expires. This article covers the patterns and the failure modes.

## Where caches live

A request passes through many places that can cache:

```text
browser cache -> CDN / edge -> reverse proxy -> application (in-process) -> distributed cache (Redis) -> database (buffer pool)
```

| Layer | Good for | Watch out for |
|---|---|---|
| Browser (HTTP headers) | Static assets, public pages | You can't purge it — use versioned URLs |
| CDN / edge | Static files, public pages, API responses identical for everyone | Never cache personalised responses publicly |
| In-process memory | Tiny, hot, rarely changing data (config, feature flags) | Each instance has its own copy; memory limits |
| Distributed (Redis, Memcached, Valkey) | Shared across instances: query results, sessions, computed views | Network hop, another system to run |
| Database buffer pool | Automatic | Already there — make sure the working set fits in RAM |

The cheapest cache is often HTTP: a `Cache-Control: public, max-age=60` header on a public page lets
a CDN absorb almost all of its traffic.

## The main patterns

### Cache-aside (lazy loading) — the default

The application manages the cache explicitly:

```python
def get_product(product_id):
    key = f"product:{product_id}:v1"
    cached = redis.get(key)
    if cached is not None:
        return deserialize(cached)                 # hit
    product = db.query_product(product_id)         # miss: go to the source
    redis.set(key, serialize(product), ex=300)     # store with a TTL (5 min)
    return product

def update_product(product_id, changes):
    db.update_product(product_id, changes)
    redis.delete(f"product:{product_id}:v1")       # invalidate; next read reloads
```

- Only data that is actually read gets cached.
- A cache outage degrades performance but not correctness (reads fall through to the DB).
- First reads after a change or expiry are slow (a miss).

### Read-through

Like cache-aside, but the cache library/service loads from the database itself on a miss. Same
behaviour, logic hidden behind the cache client.

### Write-through

Every write goes to the cache *and* the database synchronously. The cache is always warm and fresh for
data that was written, at the cost of slower writes and caching data that may never be read.

### Write-behind (write-back)

Writes go to the cache, and the cache flushes them to the database asynchronously, in batches. Very
fast writes and great for absorbing bursts (counters, analytics) — but if the cache dies before
flushing, **data is lost**. Use only for data you can afford to lose or rebuild.

## Invalidation: the hard part

> "There are only two hard things in Computer Science: cache invalidation and naming things."
> — Phil Karlton

When the source data changes, the cached copy is wrong until it is updated or removed. Strategies,
from simplest to most precise:

1. **TTL only.** Accept staleness up to the TTL. Perfect for data where "up to 60 seconds old" is fine
   (view counts, recommendations, public listings). Always set a TTL anyway, as a safety net.
2. **Delete on write.** After updating the database, delete the cache key. Prefer **deleting** over
   *setting* the new value: two concurrent writers that each `SET` can leave the older value in the
   cache.
3. **Versioned keys.** Put a version in the key (`product:42:v17`) and bump it on change; old entries
   simply expire. Also great for deploys that change the cached format (`:v1` → `:v2`).
4. **Event-driven invalidation.** Publish change events (or use change data capture on the database)
   and let a consumer invalidate affected keys — useful when many services cache the same data.

A subtle race even with delete-on-write:

```text
reader: cache miss -> reads OLD value from DB ............................ SET cache = OLD (stale!)
writer:                      updates DB to NEW -> DELETE cache key
```

The reader fetched before the update and wrote after the delete. Mitigations: short TTLs as a
backstop, a delayed second delete ("double delete"), or version checks on write. For most data, a
TTL bounding the damage is enough. For data that must never be stale (balances, inventory you sell
against), **don't cache it** — or read it from the database inside the transaction that depends on it.

## Stampedes: when the cache protects the database a little too well

Imagine the home page query takes 200 ms and is cached for 60 s, serving 1,000 requests/second.
At the moment the key expires, every request in the next 200 ms misses — and **200 identical
queries** hit the database at once. With a heavier query or more traffic, the database falls over, the
queries take longer, more requests miss, and the cache never gets refilled. This is a **cache
stampede** (or thundering herd, or dog-piling).

Fixes:

- **Request coalescing / single flight.** Within one process, only one request recomputes a given
  key; the others wait for its result. (Discord used this idea in its data services — see the
  [case study](/posts/discord-message-storage-case-study).)
- **A distributed lock on recompute.** The first miss takes a short Redis lock
  (`SET key:lock 1 NX PX 5000`) and recomputes; others briefly serve stale data or wait and retry.
- **Stale-while-revalidate.** Store a soft expiry inside the value. After the soft expiry, one request
  refreshes in the background while everyone keeps getting the slightly stale value. HTTP supports
  this natively: `Cache-Control: max-age=60, stale-while-revalidate=30`.
- **Probabilistic early refresh.** Each request has a small, growing chance of refreshing the value
  *before* it expires, so refreshes spread out instead of happening at one instant.
- **Jittered TTLs.** If you warm 10,000 keys at once with `ex=3600`, they all expire together. Add
  randomness: `ex = 3600 + random(0, 300)`.

## Other failure modes

- **Cache penetration**: requests for keys that don't exist (e.g. random ids from a scraper) always
  miss and hit the database. Cache the "not found" result briefly too, or check a Bloom filter.
- **Hot keys**: one key (a celebrity's profile) receives so many requests that a single Redis shard
  saturates. Add a small in-process cache in front, or replicate the key under several names
  (`key#1..key#8`) and pick one at random.
- **Cold start**: after a cache restart everything misses at once. Warm the most important keys first,
  or let traffic in gradually.
- **Memory pressure and eviction**: configure an eviction policy (e.g. Redis `allkeys-lru`) and
  monitor the hit ratio; a falling hit ratio is an early warning.
- **Caching personalised data under a shared key** — the classic data leak. Include the user (or
  tenant) id in keys for anything user-specific, and mark such HTTP responses `private`.

## What (not) to cache

Good candidates: data read far more often than written; expensive aggregates; responses from slow
third-party APIs; rendered fragments; sessions.

Poor candidates: data that must be exactly current; data that is cheap to compute; data that is
rarely read twice; anything you can't safely serve stale.

Before adding a cache, check whether an **index**, a better query or a **read replica** solves the
problem — every cache adds a consistency problem you now have to think about.

## Checklist

- [ ] Every key has a TTL (with jitter).
- [ ] Keys include a version and, for personal data, the user/tenant id.
- [ ] Writes invalidate (delete) affected keys after the database commit.
- [ ] Hot or expensive keys are protected against stampedes.
- [ ] The app still works (slower) if the cache is down.
- [ ] Hit ratio, latency and evictions are monitored.

## Further reading

- AWS whitepaper: [Database Caching Strategies Using Redis](https://docs.aws.amazon.com/whitepapers/latest/database-caching-strategies-using-redis/welcome.html)
- MDN: [HTTP caching](https://developer.mozilla.org/en-US/docs/Web/HTTP/Caching) and [Cache-Control](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control)
- Redis docs: [Key eviction](https://redis.io/docs/latest/develop/reference/eviction/)
- Vattani, Chierichetti, Lowenstein — *Optimal Probabilistic Cache Stampede Prevention* (VLDB 2015)
