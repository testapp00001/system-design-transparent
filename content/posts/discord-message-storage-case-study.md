+++
title = "Case study: how Discord stores trillions of messages (MongoDB → Cassandra → ScyllaDB, Go → Rust)"
summary = "A guided tour of Discord's public engineering posts: why they left MongoDB, how they modelled messages in Cassandra, what broke at 177 nodes, and how Rust data services and ScyllaDB fixed tail latency."
tags = ["case-study", "database", "nosql", "scalability", "performance"]
level = "advanced"
date = 2026-10-02
+++

Discord has written unusually candid posts about how its message storage evolved. Read together, they
form one of the best real-world lessons in data modelling, tail latency and migrations. This article
summarises them and pulls out the lessons that apply to systems far smaller than Discord's.

All numbers below are as reported by Discord in the posts linked at the end.

## Stage 1 (2015): MongoDB, until the data no longer fit in RAM

Discord launched with messages in a single MongoDB replica set. By late 2015 they had about **100
million messages**, and the data and indexes no longer fit in memory. Latency became unpredictable —
the classic symptom of a working set that has outgrown RAM (see
[how databases store data](/posts/how-databases-store-data)).

Their requirements for a replacement, paraphrased from the 2017 post:

- **Linear scalability** — add nodes, get capacity, no manual re-sharding.
- **Automatic failover** — survive node loss without waking people up.
- **Low maintenance** once set up.
- **Predictable performance** under load.
- Proven technology and open source.

They chose **Apache Cassandra**.

## Stage 2 (2016–2017): modelling messages for Cassandra

Cassandra is a wide-column store: rows are distributed across nodes by a **partition key**, and within
a partition they are sorted by a **clustering key**. You design tables around queries — there are no
joins and no ad-hoc filtering. (See [choosing a database](/posts/choosing-a-database).)

Discord's main query is "the most recent messages in a channel", plus paging backwards. So:

```sql
CREATE TABLE messages (
  channel_id bigint,
  bucket     int,          -- a fixed time window (Discord used ~10 days)
  message_id bigint,       -- a Snowflake: time-ordered, unique
  author_id  bigint,
  content    text,
  PRIMARY KEY ((channel_id, bucket), message_id)
) WITH CLUSTERING ORDER BY (message_id DESC);
```

Three design decisions worth stealing:

1. **Snowflake ids.** A 64-bit id containing a timestamp plus worker and sequence bits. Ids are unique
   without coordination and sort by time, so "newest first" is just the clustering order.
2. **Partition = channel + time bucket.** Partitioning by channel alone would let a busy channel's
   partition grow without limit (Cassandra handles large partitions poorly). The time bucket caps
   partition size; reading recent history touches one or two buckets.
3. **Model for the read path.** One query, one partition, rows already in the right order.

The 2017 post is also frank about surprises: Cassandra's eventually consistent writes interacting badly
with concurrent edits and deletes, and **tombstones** (deletion markers) making reads of
heavily-deleted ranges slow. Every database has sharp edges; you only learn them in production.

## Stage 3 (2017–2022): 12 nodes become 177, and the problems become operational

By early 2022 the messages cluster had grown to **177 Cassandra nodes** holding **trillions** of
messages. Discord described the pain points:

- **Hot partitions.** A huge server with a very active channel concentrates reads on one partition,
  and therefore on the handful of nodes that replicate it. Those nodes slow down, and because queries
  wait for them, latency spreads across the whole cluster.
- **Reads are more expensive than writes.** Cassandra (an LSM-tree store) writes cheaply to memory and
  an append-only log, but a read may have to merge data from the memtable and several on-disk SSTables.
  A read-heavy hot spot is the worst case.
- **Compaction falling behind.** The background merging of SSTables couldn't keep up, which made reads
  more expensive still.
- **Garbage collection pauses.** Cassandra runs on the JVM; GC pauses produced significant latency
  spikes, sometimes requiring an operator to take a node out of rotation.
- **Toil.** Maintenance and on-call firefighting consumed a lot of engineering time.

Notice: the database *scaled* — they had trillions of messages. What hurt was **tail latency** (the
slowest requests) and **operational cost**. At scale, p99 latency and on-call load matter as much as
raw capacity.

## The detour: why Discord rewrote a service from Go to Rust (2020)

Before the big migration, a smaller story illustrates the GC problem clearly. Discord's **Read
States** service tracks which channels and messages each user has read; it is hit on every connect,
every message sent and every message read. It kept a large in-memory LRU cache of read states and was
written in Go.

Every two minutes, the service showed **latency spikes**. Go's garbage collector runs at least every
two minutes, and each run had to scan the huge cache — even though very little garbage was being
produced. Tuning didn't remove the spikes.

They rewrote the service in **Rust**, which has no garbage collector: memory is freed deterministically
when the owner goes out of scope, such as when an entry is evicted from the LRU cache. The Rust
version had no periodic spikes and better latency, CPU and memory usage — and then they increased the
cache capacity, improving it further.

The lesson is not "Go is bad". It's that **a GC's cost grows with the size of the live heap**, so a
service whose whole job is to keep a giant in-memory structure is a poor fit for a GC'd runtime. Most
services don't look like that.

## Stage 4 (2022): fix the access pattern before changing the database

Discord's next step was not "swap the database". First, they built **data services**: Rust services
sitting between their API and the database, exposing one gRPC endpoint per query. Two features made
them valuable:

- **Request coalescing.** If many users request the same row at the same moment — exactly what
  happens when a huge server pings `@everyone` and thousands of clients load the same channel — the
  data service makes **one** database query and shares the result with all waiting requests.
- **Consistent-hash routing.** Requests are routed by `channel_id`, so all requests for the same channel
  hit the same data-service instance, which makes coalescing effective.

```text
 thousands of clients ---> API ---> data service (routed by channel_id)
 ask for channel 123                 |  1 query for all concurrent identical requests
                                     v
                                 database
```

This flattened hot partitions *before* any database change. It's a pattern you can use with any
database: coalescing (sometimes called "single flight") is a few dozen lines of code. See also
[caching strategies](/posts/caching-strategies) for the related cache-stampede problem.

## Stage 5 (2022): Cassandra → ScyllaDB

**ScyllaDB** is a Cassandra-compatible database written in C++: same data model and query language,
but no JVM garbage collector, and a **shard-per-core** architecture where each CPU core owns a slice of
data and runs without locks. Discord had already moved its other databases to ScyllaDB and had
learned to operate it; messages were last.

The migration:

- Discord's first plan, using ScyllaDB's Spark-based migrator, was estimated to take about three
  months.
- Instead they wrote a migrator **in Rust**, reusing their data-service code. It read token ranges
  from Cassandra, checkpointed progress locally in SQLite, and wrote to ScyllaDB — reaching about
  **3.2 million messages per second**. The bulk migration took **nine days**.
- New writes went to both databases during the migration, and the data services made the cutover
  invisible to the rest of the system.

Results Discord reported:

| | Cassandra | ScyllaDB |
|---|---|---|
| Nodes | 177 | 72 |
| Storage per node | ~4 TB average | 9 TB |
| p99 read latency | 40–125 ms | ~15 ms |
| p99 insert latency | 5–70 ms | ~5 ms, steady |

During the 2022 World Cup final, message traffic spiked with every goal, and the system absorbed it
without trouble.

## Lessons for the rest of us

1. **Fit the working set in memory, or plan for when it won't.** Discord's first migration was
   triggered by exactly that.
2. **Model data for your read path.** Partition + clustering key chosen from the main query; bucket
   partitions so none grow unbounded.
3. **Time-sortable ids** (Snowflake, UUIDv7, ULID) solve ordering and uniqueness without coordination.
4. **Watch tail latency, not averages.** Users experience the p99; at scale, the slowest node defines
   everyone's latency.
5. **GC pauses scale with live heap size.** Large in-memory caches in GC'd runtimes cause periodic
   pauses.
6. **Put a layer in front of the database.** Data services gave Discord request coalescing, hot-spot
   protection and a seamless migration path. Even a small app benefits from one module that owns all
   access to a table.
7. **Migrate with dual writes, checkpoints and a fast custom tool** when the stock tool is too slow.
8. **You are probably not Discord.** These are solutions to problems at trillions of rows and millions
   of concurrent users. A single PostgreSQL server comfortably handles the message volume of most
   products. Learn the *reasons* behind these decisions, and apply them when your measurements say so.

## Sources

- Discord (2017): [How Discord Stores Billions of Messages](https://discord.com/blog/how-discord-stores-billions-of-messages)
- Discord (2020): [Why Discord is switching from Go to Rust](https://discord.com/blog/why-discord-is-switching-from-go-to-rust)
- Discord (2023): [How Discord Stores Trillions of Messages](https://discord.com/blog/how-discord-stores-trillions-of-messages)
- ScyllaDB docs: [Architecture overview](https://docs.scylladb.com/)
