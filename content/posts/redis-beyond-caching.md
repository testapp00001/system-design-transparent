+++
title = "Redis beyond caching: data structures, streams, pub/sub and persistence"
summary = "What Redis offers besides a cache: data structures and their real uses, streams and pub/sub, atomic operations, persistence and what it can lose, Sentinel and Cluster, eviction, the 2024 licence change, and when not to use it as your main database."
tags = ["caching", "nosql", "backend"]
level = "intermediate"
date = 2026-10-02
+++

Most teams first use Redis as a cache: store the result of a slow query, read it back in under a
millisecond. A year later, the same Redis instance also holds sessions, rate-limit counters, a job
queue, a leaderboard and a list of who is online. Then it restarts during maintenance, and the team
finds out which of those things were safe to lose. This article explains what Redis offers beyond
caching, how it works, and what can go wrong.

## What Redis is

Redis is an **in-memory data structure server**: a large hash map that all your servers share, where
each value has a **type** (string, hash, list, set, sorted set, stream...). Commands change the value
on the server. You don't read a list, append in your code and write it back; you send `RPUSH`.

Two design choices explain most of its behaviour:

- **Data lives in RAM.** Most operations take well under a millisecond. The disk is only used to
  survive restarts. Your data must fit in memory.
- **Commands run one at a time** on one main thread (newer versions can use extra threads for network
  I/O, but not to run commands). So every command is atomic without locks, but one slow command,
  such as `KEYS *` on millions of keys, makes every client wait.

> [!NOTE]
> **Valkey** is a fork of Redis (see [licensing](#licensing)). The basics here apply to both.

## Data structures and their real uses

| Type | Think of it as | Main commands | Typical uses |
|---|---|---|---|
| String | Text, number or bytes | `SET`, `GET`, `INCR` | Cache entries, counters, simple locks |
| Hash | A small object with fields | `HSET`, `HINCRBY` | User profile, per-object counters |
| List | A queue open at both ends | `LPUSH`, `BLMOVE`, `LTRIM` | Simple job queues, "latest 100 items" |
| Set | Unique members, no order | `SADD`, `SINTER` | Tags, "who liked this" |
| Sorted set | Members ordered by a score | `ZADD`, `ZRANGE` | Leaderboards, rate limiters, delayed jobs |
| Stream | Append-only log | `XADD`, `XREADGROUP` | Event logs, work queues |
| HyperLogLog | Approximate distinct counter | `PFADD`, `PFCOUNT` | Unique visitors |
| Bitmap | One bit per integer id | `SETBIT`, `BITCOUNT` | Daily active users |
| Geospatial | Points on a map | `GEOADD`, `GEOSEARCH` | "Drivers within 2 km" |

**Strings and counters.** A string holds up to 512 MB of any bytes. `INCR` adds to a number
atomically, so fifty servers can increment one counter at once without losing an update. If your
code does `GET`, adds 1, then `SET`, two servers can read the same old value and one update is lost.

**Lists** make simple queues. `BLMOVE` waits for an item and moves it into a "processing" list in one
step, so a crashed worker does not silently lose the job. Sidekiq (Ruby) and BullMQ (Node.js) build
job queues on Redis (see [background jobs](/posts/background-jobs-and-cron)).

**Sorted sets** keep members ordered by a numeric score. Adding, updating and finding a rank are
O(log N), so they stay fast with millions of members:

```text
> ZADD leaderboard 1500 ana 1720 ben 980 chen
(integer) 3
> ZINCRBY leaderboard 50 chen
"1030"
> ZRANGE leaderboard 0 1 REV WITHSCORES     # top 2 (ZREVRANGE on old versions)
1) "ben"
2) "1720"
3) "ana"
4) "1500"
```

With a timestamp as the score, it becomes a **delayed job queue** or a **sliding-window rate
limiter** (shown below).

### Streams: a log with consumer groups

A stream is an append-only log. Each entry gets an ID like `<milliseconds>-<sequence>` and **stays
stored** until you trim it. A **consumer group** lets several workers share the work:

```text
producer --XADD--> stream "orders":  [101] [102] [103] [104] [105]  (newest)

group "billing"  (own position; each entry goes to ONE worker in the group)
  worker-1: got 102, 103 -> XACK when done
  worker-2: got 104      -> crashed before XACK: 104 stays pending, can be claimed

group "search"   (reads the same entries again, independently)
```

```text
XADD orders MAXLEN ~ 1000000 * order_id 1042 total 2000
XGROUP CREATE orders billing $ MKSTREAM
XREADGROUP GROUP billing worker-1 COUNT 10 BLOCK 5000 STREAMS orders >
XACK orders billing 1790914191338-0
XAUTOCLAIM orders billing worker-2 60000 0-0 COUNT 10   # claim entries pending > 60 s
```

Delivery is **at least once**: a worker can crash after the work but before `XACK`, so make
consumers [idempotent](/posts/retries-timeouts-and-idempotency). Trim with `MAXLEN`, because the
stream lives in RAM. For long retention and very high volume, use a log like Kafka (see
[message queues and event streams](/posts/message-queues-and-event-streams)).

### Pub/sub: fire and forget

`PUBLISH room:7 "hi"` delivers the message to every client **subscribed right now**. Nothing is
stored: if nobody listens, it is gone (`PUBLISH` returns the number of receivers, possibly 0), and a
subscriber that reconnects after a deploy misses everything in between. Slow subscribers are
disconnected when their output buffer passes a limit.

| | Pub/sub | List queue | Stream |
|---|---|---|---|
| Stored? | No | Until popped | Until trimmed |
| Each message goes to | Every current subscriber | One worker | One worker per group |
| Offline consumer | Misses it | Gets it later | Gets it later, can replay |

Use pub/sub for signals that can be lost: "room 7 has a new message, go fetch it", cache invalidation,
or fan-out to [WebSocket servers](/posts/scaling-websockets-chat). Store the real data first, then
publish (more in [pub/sub for real-time systems](/posts/pub-sub-redis-nats)). In Redis Cluster,
normal pub/sub messages go to every node; Redis 7.0 added **sharded pub/sub** (`SPUBLISH`).

### HyperLogLog, bitmaps and geospatial

- **HyperLogLog** counts distinct items approximately. The Redis docs state it uses at most 12 KB per
  key, with a standard error of 0.81%. You cannot list the members.
- **Bitmaps** are bit operations on a string. Bit *n* means "user *n* was active today":
  `SETBIT active:2026-10-02 42 1`, then `BITCOUNT`. One million user ids fit in about 125 KB. Use
  dense integer ids: setting bit 4,000,000,000 creates a 500 MB string.
- **Geospatial** commands store longitude/latitude in a sorted set; `GEOSEARCH` finds members in a
  radius, sorted by distance.

## TTLs: data that cleans itself up

`SET key value EX 3600` or `EXPIRE key 3600` gives a key a **time to live** (TTL) in seconds. Redis
deletes expired keys when they are accessed, and a background job also checks random keys with a TTL.
Three surprises:

- Expiry is **per key**. Items inside a list or set cannot expire on their own (Redis 7.4 added
  per-field expiry for hashes only).
- A plain `SET` on an existing key **removes its TTL** unless you add `KEEPTTL`. `INCR` keeps it.
- `INCR` then `EXPIRE` is two commands. If your process dies between them, the counter never
  expires. Do both atomically.

## Atomicity without locks

Every single command is atomic. To combine several:

1. **`MULTI` / `EXEC`** runs queued commands together; no other client's command runs in between. It
   is **not** a rollback transaction: if one command fails while running (for example, wrong type),
   the others still apply. For check-then-write, use `WATCH`: `EXEC` fails if a watched key
   changed, and you retry.
2. **Lua scripts** (`EVAL`, and Functions since Redis 7.0) run on the server as one atomic step and
   can read, decide and write. A sliding-window rate limiter on a sorted set:

   ```lua
   -- KEYS[1] = limiter key; ARGV = now_ms, window_ms, limit, unique request id
   local key, now = KEYS[1], tonumber(ARGV[1])
   local window, limit = tonumber(ARGV[2]), tonumber(ARGV[3])
   redis.call('ZREMRANGEBYSCORE', key, 0, now - window)   -- forget old requests
   if redis.call('ZCARD', key) >= limit then return 0 end
   redis.call('ZADD', key, now, ARGV[4])
   redis.call('PEXPIRE', key, window)                    -- clean up idle users
   return 1
   ```

   Pass every key the script touches in `KEYS` (Redis Cluster needs this). Keep scripts short: while
   one runs, nothing else does. See [rate limiting](/posts/rate-limiting).
3. **Pipelining** (sending many commands without waiting for each reply) saves round trips, but is
   **not** atomic.

For locks with `SET key token NX PX 30000`, see [distributed locks](/posts/distributed-locks).

## Persistence: what you lose on a restart

| | RDB snapshot | AOF (append-only file) |
|---|---|---|
| What it writes | A point-in-time copy of all data | Every write command, replayed at startup |
| In the default `redis.conf` | On (the `save` line below) | Off (`appendonly no`) |
| Lost in a crash | Everything since the last snapshot (minutes) | With `everysec`: about the last second |
| Restart | Fast to load | Slower; compacted by background rewrites |

A setup for data you care about:

```text
appendonly yes
appendfsync everysec          # fsync every second (default); "always" is slower
aof-use-rdb-preamble yes      # faster restarts
save 3600 1 300 100 60 10000  # RDB snapshots too: copy them off the machine
```

Defaults differ between packages, Docker images and managed services: check `CONFIG GET save` and
`CONFIG GET appendonly`. Also:

- Snapshots and AOF rewrites **fork** the process. Pages that change during the save are copied, so
  a busy instance needs spare RAM while saving.
- **Replication is asynchronous.** A write the primary confirmed may not have reached a replica when
  the primary dies; after failover it is gone, whatever your fsync setting. `WAIT` narrows this
  window, but the Redis docs say it does not make Redis strongly consistent.

## Replication, Sentinel and Cluster

- **Replication:** a primary streams writes to read-only replicas, which can serve slightly stale
  reads. Nothing fails over automatically (see
  [replication and high availability](/posts/replication-and-high-availability)).
- **Sentinel:** separate processes watch the primary. When enough agree it is down, they promote a
  replica and tell clients the new address. Run at least three Sentinels on separate machines. All
  data still lives on one primary.
- **Redis Cluster:** data is split across several primaries, each with replicas and built-in
  failover. There are **16,384 hash slots**: `slot = CRC16(key) mod 16384`, and each primary owns
  some. A node asked about a slot it does not own replies `MOVED` with the right address.

```text
  "user:42:cart" --CRC16 mod 16384--> slot 12984

   slots 0-5460         slots 5461-10922      slots 10923-16383
  +-------------+       +-------------+       +-------------+
  |  primary A  |       |  primary B  |       |  primary C  |  <- owns slot 12984
  +------+------+       +------+------+       +------+------+
         |                     |                     |
    [replica A1]          [replica B1]          [replica C1]
```

**Multi-key commands** (`MGET`, `MULTI`/`EXEC`, scripts with several keys) only work when all keys
are in the **same slot**; otherwise you get `CROSSSLOT Keys in request don't hash to the same slot`.
**Hash tags** fix this: if a key contains `{...}`, only the part inside the braces is hashed, so
`user:{42}:profile` and `user:{42}:cart` share a slot. Don't put everything under one tag, or one node
gets all the traffic.

Start with a primary, a replica and Sentinel (or a managed service). Move to Cluster when data no
longer fits one machine's RAM or one main thread is too busy (see
[sharding](/posts/sharding-and-partitioning)).

## Memory and eviction

`maxmemory` sets a memory limit. On 64-bit systems the default is `0`, no limit: Redis grows until the
machine runs out of memory. `maxmemory-policy` decides what happens at the limit:

| Policy | What it removes | Use for |
|---|---|---|
| `noeviction` (default) | Nothing; writes fail with an error | Queues, sessions |
| `allkeys-lru`, `allkeys-lfu` | Least recently / least frequently used key | A pure cache |
| `volatile-lru`, `volatile-lfu`, `volatile-ttl` | Only keys that have a TTL | Mixed data (fragile) |
| `allkeys-random`, `volatile-random` | Random keys | Rarely the best choice |

LRU and LFU are approximate: Redis samples a few keys and evicts the best candidate. The safest design
is **separate instances per role**: an `allkeys-lru` cache, and a `noeviction` instance for queues and
sessions. Find large keys with `redis-cli --bigkeys` and delete them with `UNLINK`, which frees
memory in the background.

## Licensing: Redis, Valkey and the 2024 change {#licensing}

Redis was open source under the BSD licence until 2024. In March 2024, Redis Ltd. announced that
versions from 7.4 on would use a choice of two **source-available** licences (RSALv2 or SSPLv1),
which are not open source by the OSI definition. Soon after, the Linux Foundation announced
**Valkey**, a fork of the last BSD-licensed version (7.2.4), supported by companies including AWS,
Google Cloud and Oracle. In 2025, Redis 8 added the AGPLv3, an OSI-approved open source licence, as a
third option.

If you only use Redis inside your own application, the practical questions are which server your
cloud provider or Linux distribution ships, and which newer features you need. If your company sells
Redis as a service or redistributes it, ask your legal team to read the licences.

## When not to use Redis as your primary database

Redis is a fine *only* home for data you can lose or rebuild: caches, sessions (users log in again),
rate-limit counters, presence, leaderboards you can recompute. Keep the source of truth elsewhere when:

- **Losing a write is unacceptable** (orders, payments). Snapshots lose minutes, and failover can
  lose confirmed writes even with AOF.
- **You will need queries you did not plan.** There is no SQL and no joins; secondary indexes are
  sets you keep in sync by hand.
- **The data is larger than RAM**, which costs far more per GB than disk.
- **You need constraints** or transactions across shards.

The usual pattern: PostgreSQL (or similar) is the source of truth, and Redis holds derived data you
can rebuild, updated after the commit or through [change data capture](/posts/change-data-capture).
See [choosing a database](/posts/choosing-a-database).

## In practice: a checklist

- [ ] For every key pattern, you know if it is cache (can be lost) or data (cannot).
- [ ] `maxmemory` and the eviction policy are set on purpose, with alerts before the limit.
- [ ] Persistence matches the role: none or RDB for caches; AOF `everysec` plus RDB for queues.
- [ ] Clients use timeouts and a [connection pool](/posts/connection-pooling).
- [ ] You monitor memory, evictions, hit ratio, replication lag and `SLOWLOG`.
- [ ] Redis is not reachable from the internet and requires authentication.

## Common mistakes

- **Using pub/sub as a queue**: messages sent during a deploy are lost.
- **`KEYS *` in production.** It blocks the server; use `SCAN`.
- **`allkeys-lru` on the instance holding your job queue**: jobs vanish without an error.
- **Unbounded lists and streams**, or multi-megabyte values that make every client wait.

## Further reading

- Redis docs: [Data types](https://redis.io/docs/latest/develop/data-types/)
- Redis docs: [Persistence](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/)
- Redis docs: [Cluster specification](https://redis.io/docs/latest/operate/oss_and_stack/reference/cluster-spec/)
- Redis docs: [Key eviction](https://redis.io/docs/latest/develop/reference/eviction/)
- [Valkey](https://valkey.io/), the Linux Foundation fork
