+++
title = "Partitioning and sharding: splitting data when one table or one server is not enough"
summary = "Table partitioning inside one database vs sharding across many; choosing a shard key; hash, range and directory sharding; consistent hashing; hot spots, cross-shard queries and resharding — and what to try first."
tags = ["database", "scalability", "distributed-systems", "system-design"]
level = "advanced"
date = 2026-10-02
+++

At some point a table gets so big that indexes no longer fit in memory, maintenance takes hours, and
deleting old data hurts. Later, maybe, a single database server can no longer absorb your writes.
Partitioning and sharding are the answers — and they are among the most expensive architectural
decisions you can make, so it pays to understand them before you need them.

## Two different things

- **Partitioning** splits one table into smaller pieces **inside one database server**. The database
  routes queries to the right pieces. Your application usually doesn't change.
- **Sharding** splits data across **multiple database servers**, each holding a subset (a shard). Your
  application or a routing layer must know where each row lives.

People also say "horizontal partitioning" (split rows) vs "vertical partitioning" (split columns or
tables, e.g. moving large blobs or a whole feature's tables to another database).

## Partitioning inside one database (try this first)

PostgreSQL has declarative partitioning (MySQL has a similar feature):

```sql
CREATE TABLE events (
    id         BIGINT GENERATED ALWAYS AS IDENTITY,
    tenant_id  BIGINT      NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    payload    JSONB,
    PRIMARY KEY (id, created_at)          -- the partition key must be part of unique keys
) PARTITION BY RANGE (created_at);

CREATE TABLE events_2026_09 PARTITION OF events
    FOR VALUES FROM ('2026-09-01') TO ('2026-10-01');
CREATE TABLE events_2026_10 PARTITION OF events
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');
```

Benefits:

- **Partition pruning**: `WHERE created_at >= '2026-10-01'` only touches the October partition.
- **Cheap retention**: dropping or detaching an old partition is instant, versus a huge `DELETE` that
  bloats the table.
- Smaller indexes per partition; maintenance (VACUUM, reindex) per partition.

Costs: queries that don't filter on the partition key scan all partitions; unique constraints must
include the partition key; too many partitions (thousands) slow down planning. Tools like
`pg_partman` automate creating future partitions.

Strategies: **range** (by time — the most common), **list** (by region or tenant category), **hash**
(spread evenly by key).

## When you actually need sharding

Before sharding, exhaust the cheaper options — each buys a lot of headroom:

1. **Indexes and query fixes** (often 10–1000× on the queries that matter).
2. **A bigger server.** Modern machines have hundreds of cores and terabytes of RAM.
3. **Read replicas** for read-heavy load.
4. **Caching** hot reads.
5. **Partitioning** and archiving cold data.
6. **Moving workloads out**: analytics to an OLAP store, search to a search engine, blobs to object
   storage, high-volume logs to a time-series or wide-column store.
7. **Splitting by feature** (vertical partitioning): different domains in different databases.

Shard when **writes** (or the size of hot data) exceed what one primary can handle, after all of the
above. Many large companies run surprisingly far on a single primary per domain — Figma, for example,
scaled one Postgres database vertically and split tables across databases by domain before
implementing horizontal sharding.

## Choosing a shard key

The **shard key** decides which shard a row lives on. It is the most important decision, because it's
very hard to change later.

A good shard key:

- **Is present in almost every query**, so queries go to one shard instead of all of them.
- **Keeps data that is used together on the same shard** — e.g. `tenant_id` in a B2B SaaS: all of one
  customer's data on one shard, so joins and transactions stay local.
- **Spreads load evenly** — no shard receives far more traffic or data than others.
- **Has high cardinality** — many distinct values, so data can be spread and rebalanced.

Bad shard keys: `country` (a few huge values), `created_at` alone (all new writes hit the newest shard),
a boolean, anything that changes over time (a row would have to move).

## Sharding schemes

### Range sharding

Keys `A–F` on shard 1, `G–M` on shard 2… or id ranges. Range queries are efficient, but
sequential keys (timestamps, auto-increment ids) send **all new writes to the last shard** — a hot
spot.

### Hash sharding

`shard = hash(key) mod N`. Spreads keys evenly, but range queries hit all shards, and **changing N
moves almost every key**: going from 4 to 5 shards remaps about 80% of keys.

### Consistent hashing

Place shards (and many **virtual nodes** per shard) on a ring of hash values; a key belongs to the next
node clockwise from its hash. Adding a shard moves only the keys between the new node and its
predecessor — about 1/N of the data — instead of most of it.

```text
            hash ring
         n3 .--------. n1          key k hashes here -> stored on the next node clockwise (n1)
           /     k    \
          |            |
           \          /
         n2 '--------' n4          adding n5 between n1 and n4 only moves keys in that arc
```

Used by Cassandra, DynamoDB, many caches and load balancers. Discord routes requests to data services
by consistent hashing of `channel_id` (see the [case study](/posts/discord-message-storage-case-study)).

### Directory (lookup) sharding

A lookup table maps each key (or tenant) to its shard. Most flexible: you can move a single big tenant
to its own shard. The directory itself must be highly available and cached.

A common practical design: hash keys into a **fixed, large number of logical shards** (say 4,096) and
map logical shards to physical servers in a directory. Rebalancing moves whole logical shards between
servers, never rehashing individual keys.

## What gets harder after sharding

- **Cross-shard queries**: "top 10 products across all tenants" must query every shard and merge
  (scatter-gather). Analytics should move to a separate data warehouse.
- **Cross-shard transactions**: no more single ACID transaction across shards; you need sagas or
  2PC (see [distributed transactions](/posts/distributed-transactions-saga-outbox)).
- **Unique constraints and ids**: auto-increment per shard collides. Use globally unique,
  time-sortable ids (Snowflake-style, UUIDv7).
- **Hot shards**: one huge tenant or a celebrity account. You may need to split a single key's data or
  give big tenants dedicated shards.
- **Resharding**: moving data between shards while serving traffic — copy, catch up with changes,
  switch reads/writes, verify — is a project, not a command.
- **Operations multiply**: backups, migrations, upgrades and monitoring for N databases. Schema
  migrations must be rolled out to every shard.

## Tools that do it for you

Rather than sharding in application code, consider systems that shard transparently: **Citus**
(PostgreSQL extension), **Vitess** (MySQL, originally built at YouTube), **distributed SQL** databases
(CockroachDB, YugabyteDB, TiDB, Spanner), or NoSQL stores built around partitioning (Cassandra,
DynamoDB). They don't remove the need for a good shard key, but they handle routing, rebalancing and
much of the operational work.

## Key takeaways

- Partition (inside one DB) before you shard (across DBs).
- Exhaust indexing, vertical scaling, replicas, caching and moving workloads out first.
- The shard key is the decision that matters: in every query, co-locates related data, spreads load.
- Use consistent hashing or many logical shards so you can rebalance without rehashing everything.

## Further reading

- PostgreSQL docs: [Table Partitioning](https://www.postgresql.org/docs/current/ddl-partitioning.html)
- Figma: [How Figma's databases team lived to tell the scale](https://www.figma.com/blog/how-figmas-databases-team-lived-to-tell-the-scale/)
- [Vitess documentation](https://vitess.io/docs/) and [Citus documentation](https://docs.citusdata.com/)
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapter 6 (Partitioning)
- Karger et al., *Consistent Hashing and Random Trees* (1997)
