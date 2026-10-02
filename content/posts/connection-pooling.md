+++
title = "Database connection pooling: why it exists and how to size it"
summary = "Opening a database connection is expensive and the database can only run so many at once. How connection pools work, how to size them (smaller than you think), PgBouncer's modes, and the mistakes that exhaust pools."
tags = ["database", "postgresql", "performance", "scalability"]
level = "intermediate"
date = 2026-10-02
+++

Your app is slow under load. The database CPU is at 30%. Yet requests wait seconds for... something.
Very often that something is a **database connection**: the pool is exhausted, or so many connections
are open that the database spends its time juggling them. This article explains connection pools from
first principles.

## Why connections are expensive

Opening a connection to PostgreSQL involves:

1. A TCP handshake (and usually a TLS handshake: more round trips and CPU).
2. Authentication (e.g. SCRAM: more round trips, deliberate CPU cost).
3. The server **forking a new backend process** for this connection, which allocates memory and
   builds caches as it runs queries.

That's typically a few milliseconds locally and tens of milliseconds across networks — far more than a
simple indexed query. MySQL uses a thread per connection, which is cheaper than a process, but the
handshake, auth and per-connection memory still apply.

Doing that on every HTTP request would dominate your latency. Hence: **open connections once, reuse
them** — a pool.

## How a pool works

```text
 request handlers                         pool (max 10)                    database
 [req 1] --acquire--> [conn 1 busy] ------------------------------------> [backend process 1]
 [req 2] --acquire--> [conn 2 busy] ------------------------------------> [backend process 2]
 [req 3] --acquire--> [conn 3 idle -> busy] ----------------------------> [backend process 3]
 [req 11] --acquire--> (all busy: wait up to acquire_timeout, then error)
 handler done --release--> connection goes back to the pool, reused by the next request
```

Key settings in almost every pool (HikariCP, sqlx, pgx, SQLAlchemy, Prisma, Sequelize, ...):

| Setting | Meaning | Guidance |
|---|---|---|
| `max` size | Most connections this process will open | Small — see below |
| `min` idle | Connections kept open when idle | Low; avoids cold starts |
| acquire timeout | How long a request waits for a free connection | Short (1–5 s): fail fast instead of piling up |
| idle timeout | Close connections unused this long | Minutes |
| max lifetime | Recycle connections after this long | Below any firewall/proxy/DB idle cut-off; spreads reconnections |
| health check | Validate before use / periodically | On, cheaply |

## Sizing: smaller than you think

The database can only *execute* as many queries in parallel as it has CPU cores (and disk channels).
Beyond that, extra connections don't add throughput — they add context switching, lock contention and
memory pressure, and **latency goes up**. A widely cited starting point, from the PostgreSQL community
and popularised by HikariCP's documentation:

```text
connections ≈ (number of CPU cores × 2) + effective spindle count
```

For an 8-core database server with SSDs, that suggests something like 16–20 *active* connections
**in total**, across all app instances. Then measure and adjust.

You can sanity-check with **Little's law**: concurrency = throughput × latency. If your app runs
2,000 queries/second averaging 4 ms each, on average only 2,000 × 0.004 = **8 connections** are busy.
A pool of 100 would mostly sit idle — or worse, let a traffic spike push 100 concurrent queries at the
database at once.

### Count connections across the whole fleet

```text
total connections = app instances × pool max  (+ workers, cron jobs, admin tools, migrations)
```

Ten app instances with a pool of 20 = 200 connections. PostgreSQL's default `max_connections` is 100.
Autoscaling makes this worse: scaling out under load **increases** database connections exactly when
the database is busiest. Serverless functions are the extreme case — each concurrent function instance
may open its own connection.

## External poolers: PgBouncer and friends

When many app processes need to share a small number of real database connections, put a pooler
between them: **PgBouncer**, pgcat, Odyssey, Supavisor, or a managed proxy (e.g. Amazon RDS Proxy).
Thousands of cheap client connections are multiplexed onto a few dozen server connections.

PgBouncer has three modes:

- **Session pooling** — a client keeps a server connection for its whole session. Safe, but doesn't
  multiplex much.
- **Transaction pooling** — a client gets a server connection only for the duration of a transaction.
  This is where the big win is. But session state doesn't survive between transactions, so these
  break or need care: `SET` session variables, session-level advisory locks, `LISTEN/NOTIFY`, temporary
  tables, and protocol-level prepared statements (PgBouncer added support for these in transaction mode
  in version 1.21 via `max_prepared_statements`; older setups had to disable them in the driver).
- **Statement pooling** — per statement; multi-statement transactions are not allowed. Rarely used.

## Common mistakes that exhaust pools

- **Holding a connection during slow non-database work.** The classic: open a transaction, then call a
  payment API that takes 3 seconds. That connection is unusable for 3 seconds, and under load the pool
  drains. Do external calls *outside* transactions.
- **Leaks**: a code path that acquires a connection and never releases it (an early return, an
  exception). Use your language's scoped resource handling (`with`, `using`, `defer`, RAII) and enable
  leak detection in the pool if available.
- **N+1 queries**: 1 query for a list, then 1 per item. Each query is fast, but 200 of them per request
  occupy a connection for a long time. Batch them (`WHERE id = ANY(...)`, joins, ORM eager loading).
- **Huge pools "to be safe"**, which just move the queue from your app into the database where it's
  more expensive.
- **No acquire timeout**, so requests wait forever instead of failing fast and letting load shedding
  work.
- **A pool per request or per tenant** — accidentally creating pools dynamically.

## What to monitor

- Pool: active, idle, waiting count, and **acquire wait time**. Waiting > 0 regularly means the pool
  (or the database) is the bottleneck.
- Database: total connections vs `max_connections`; `pg_stat_activity` states — many
  `idle in transaction` sessions point to code holding transactions open.

```sql
SELECT state, count(*) FROM pg_stat_activity GROUP BY state;
```

## Key takeaways

- Pools exist because connections are expensive to open and the database can only work on a few
  queries at a time.
- Size pools **small**, and think in terms of the total across all instances.
- Fail fast with an acquire timeout; never hold a connection while waiting on something else.
- Use an external pooler when many processes (or serverless functions) share one database.

## Further reading

- HikariCP wiki: [About Pool Sizing](https://github.com/brettwooldridge/HikariCP/wiki/About-Pool-Sizing)
- PostgreSQL wiki: [Number Of Database Connections](https://wiki.postgresql.org/wiki/Number_Of_Database_Connections)
- [PgBouncer documentation](https://www.pgbouncer.org/config.html) — pool modes and settings
- PostgreSQL docs: [Monitoring statistics, including pg_stat_activity](https://www.postgresql.org/docs/current/monitoring-stats.html)
