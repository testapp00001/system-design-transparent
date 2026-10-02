+++
title = "Database indexes and reading EXPLAIN: making slow queries fast"
summary = "How B-tree indexes find rows, why column order in composite indexes matters, covering and partial indexes, when indexes hurt, and how to read a query plan to see what the database is really doing."
tags = ["database", "postgresql", "mysql", "performance"]
level = "intermediate"
date = 2026-10-02
+++

Most slow web applications are slow because of a handful of database queries, and most slow queries
are slow because of a missing or unusable index. Learning to read a query plan is one of the
highest-leverage skills a backend developer can have.

## What an index is

An index is a separate data structure, kept in sync with the table, that lets the database find rows
without reading the whole table. The default kind in PostgreSQL and MySQL is the **B-tree**: a sorted,
shallow tree (see [how databases store data](/posts/how-databases-store-data)). Because it is sorted,
it answers:

- equality: `WHERE email = 'a@b.c'`
- ranges: `WHERE created_at >= '2026-01-01'`
- prefix matches: `WHERE name LIKE 'Ann%'` (not `'%ann%'`)
- sorting: `ORDER BY created_at DESC LIMIT 20` — read the index in order and stop after 20 rows.

Without a usable index, the database does a **sequential scan**: read every row, check the condition.
On 1,000 rows that's nothing. On 50 million rows it's seconds, and it pushes useful data out of the
cache.

## Reading EXPLAIN

Ask the database how it will run a query:

```sql
EXPLAIN ANALYZE
SELECT * FROM orders WHERE customer_id = 42 ORDER BY created_at DESC LIMIT 20;
```

PostgreSQL output without a suitable index (simplified):

```text
Limit  (actual time=812.4..812.4 rows=20 loops=1)
  -> Sort  (actual time=812.4..812.4 rows=20 loops=1)
        Sort Key: created_at DESC
        -> Seq Scan on orders  (actual time=0.03..809.9 rows=1312 loops=1)
              Filter: (customer_id = 42)
              Rows Removed by Filter: 4998688
Execution Time: 812.6 ms
```

Read plans **from the innermost node outwards**. Here: scan all 5 million rows, throw away 4,998,688,
sort the remaining 1,312, keep 20. After `CREATE INDEX ON orders (customer_id, created_at);`:

```text
Limit  (actual time=0.04..0.09 rows=20 loops=1)
  -> Index Scan Backward using orders_customer_id_created_at_idx on orders
        (actual time=0.04..0.08 rows=20 loops=1)
        Index Cond: (customer_id = 42)
Execution Time: 0.11 ms
```

Same result, about 7,000× faster: the index finds customer 42's rows already ordered by date, and the
database reads exactly 20 of them.

What to look for in plans:

- **Seq Scan** on a large table with a selective filter → probably a missing index.
- **Rows Removed by Filter** much larger than rows returned → the index (if any) isn't selective for
  this query.
- **Estimated vs actual rows** wildly different (`rows=10` estimated, `rows=500000` actual) → stale
  statistics (`ANALYZE`) or correlated columns; the planner is choosing based on wrong numbers.
- **Sort** with `external merge Disk` → not enough `work_mem`, or an index could provide the order.
- **Nested Loop** with a large outer side → many repeated lookups; check the inner side uses an index.
- `EXPLAIN (ANALYZE, BUFFERS)` shows how many pages were read from cache vs disk.

> [!WARNING]
> `EXPLAIN ANALYZE` actually **runs** the query. For `UPDATE`/`DELETE`, wrap it in a transaction and
> roll back.

MySQL has `EXPLAIN` and, since 8.0.18, `EXPLAIN ANALYZE` with similar information (look for
`type: ALL` meaning a full table scan, and the `key` and `rows` columns).

## Composite indexes: column order matters

An index on `(a, b, c)` is sorted by `a`, then by `b` within equal `a`, then by `c`. Like a phone book
sorted by last name then first name, it helps only if you know the leading columns.

| Query filter | Can use index `(customer_id, status, created_at)`? |
|---|---|
| `customer_id = 42` | Yes |
| `customer_id = 42 AND status = 'paid'` | Yes |
| `customer_id = 42 AND status = 'paid' ORDER BY created_at` | Yes, including the sort |
| `customer_id = 42 ORDER BY created_at` | Partly: filter yes, but the sort isn't free (status sits in between) |
| `status = 'paid'` | Generally no — the leading column isn't constrained |
| `created_at > now() - interval '1 day'` | No |

Rules of thumb for ordering columns:

1. Columns compared with **equality** first.
2. Then the column used for **range or sorting**.
3. Range conditions "end" the useful part of the index: after `created_at > X`, later columns can't
   narrow the search further.

## Covering indexes and index-only scans

If the index contains **every column the query needs**, the database can answer from the index alone
without visiting the table:

```sql
-- PostgreSQL: INCLUDE adds non-key payload columns to the index leaves
CREATE INDEX ON orders (customer_id, created_at) INCLUDE (total, status);

SELECT created_at, total, status FROM orders WHERE customer_id = 42 ORDER BY created_at DESC LIMIT 20;
-- -> Index Only Scan
```

In MySQL InnoDB, every secondary index already contains the primary key, and you make an index
covering by adding columns to it.

## Specialised indexes worth knowing

- **Partial index** (PostgreSQL): index only the rows you query. Tiny and fast.
  `CREATE INDEX ON jobs (run_at) WHERE done_at IS NULL;` — only pending jobs, not millions of done ones.
- **Expression index**: index a computed value. `CREATE INDEX ON users (lower(email));` makes
  `WHERE lower(email) = ...` fast (this site uses `lower(username)` for case-insensitive usernames).
- **Unique index**: enforces uniqueness — the right way to prevent duplicates, rather than "check then
  insert" in code.
- **GIN** (PostgreSQL): for "contains" queries on arrays, `JSONB` and full-text search
  (`tags @> ARRAY['database']`, `doc @> '{"status":"x"}'`, `search_vector @@ query`).
- **GiST / SP-GiST**: geometric and range types, nearest-neighbour searches, exclusion constraints.
- **BRIN**: very small indexes for huge, naturally ordered tables (append-only logs by time).
- **Hash**: equality only; rarely better than B-tree.
- **Trigram (`pg_trgm`)**: makes `LIKE '%ann%'` and fuzzy matching indexable.

## Why a query might not use your index

- **Functions on the column**: `WHERE date(created_at) = '2026-10-02'` can't use an index on
  `created_at`. Rewrite as a range: `created_at >= '2026-10-02' AND created_at < '2026-10-03'`.
- **Type mismatch / implicit casts**: comparing a text column to a number, or different collations.
- **Leading wildcard**: `LIKE '%term'`.
- **Low selectivity**: if the condition matches 40% of rows, reading the whole table sequentially is
  genuinely cheaper than jumping around via the index. The planner is right.
- **`OR` across different columns**: may need separate indexes (bitmap OR) or a `UNION`.
- **Stale statistics**: run `ANALYZE` (autovacuum normally does).
- **Offset pagination**: `OFFSET 100000` still reads and discards 100,000 rows. Use keyset pagination
  (`WHERE id < last_seen_id ORDER BY id DESC LIMIT 20`) — see
  [API design](/posts/api-design-pagination-versioning).

## Indexes are not free

Every index:

- must be updated on every `INSERT`, on `DELETE`, and on `UPDATE`s that touch it (in PostgreSQL,
  potentially on any non-HOT update);
- takes disk space and, more importantly, **memory** in the cache;
- adds work for VACUUM and replication.

So don't index every column "just in case". Find unused indexes:

```sql
-- PostgreSQL: indexes never used since statistics were last reset
SELECT relname AS table, indexrelname AS index, idx_scan, pg_size_pretty(pg_relation_size(indexrelid))
FROM pg_stat_user_indexes
WHERE idx_scan = 0
ORDER BY pg_relation_size(indexrelid) DESC;
```

(Check replicas too before dropping — an index unused on the primary may serve read queries on a
replica.)

## A practical workflow

1. **Find the slow queries**, don't guess: PostgreSQL's `pg_stat_statements` (top queries by total
   time), MySQL's slow query log / Performance Schema, or your APM tool.
2. Run `EXPLAIN (ANALYZE, BUFFERS)` on a realistic copy of production data.
3. Design an index for the **whole query** (filter + sort + selected columns), not for one column.
4. Create it without blocking writes: `CREATE INDEX CONCURRENTLY` (PostgreSQL).
5. Re-check the plan and the latency; check write overhead.
6. Periodically remove unused and duplicate indexes.

## Further reading

- [Use The Index, Luke!](https://use-the-index-luke.com/) — a free, excellent guide to SQL indexing for developers
- PostgreSQL docs: [Using EXPLAIN](https://www.postgresql.org/docs/current/using-explain.html) and [Index Types](https://www.postgresql.org/docs/current/indexes-types.html)
- MySQL docs: [Optimization and Indexes](https://dev.mysql.com/doc/refman/8.0/en/optimization-indexes.html)
- [explain.dalibo.com](https://explain.dalibo.com/) — visualise PostgreSQL plans
