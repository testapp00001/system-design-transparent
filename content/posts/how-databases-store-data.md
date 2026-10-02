+++
title = "How databases store data: MySQL (InnoDB) vs PostgreSQL"
summary = "Pages, B-trees, heaps and clustered indexes — and why the different ways MySQL and PostgreSQL lay out rows change how primary keys, updates and indexes perform."
tags = ["database", "postgresql", "mysql", "performance"]
level = "advanced"
date = 2026-10-02
+++

"Use an auto-increment primary key in MySQL." "UUIDs are slow." "Postgres needs VACUUM." "Too many
indexes slow down updates." You have probably heard rules like these. They all come from one place:
**how the database physically stores rows on disk**. Once you know the layout, the rules stop being
folklore and you can predict performance yourself.

This article compares the two most popular open-source relational databases. They speak almost the
same SQL, but under the hood they made opposite design choices.

## Everything is pages

A database never reads "one row" from disk. It reads a **page** (also called a block): a fixed-size
chunk that holds many rows.

| | PostgreSQL | MySQL (InnoDB) |
|---|---|---|
| Page size | 8 KB | 16 KB (default) |
| In-memory page cache | `shared_buffers` (+ OS page cache) | `innodb_buffer_pool_size` |
| Durability log | WAL (write-ahead log) | redo log (+ binlog for replication) |

Two consequences follow immediately:

1. **Locality matters.** If the 50 rows a query needs live on 2 pages, that is 2 reads. If they are
   spread over 50 pages, it is 50 reads. On a cold cache, that is the difference between
   microseconds and many milliseconds.
2. **Memory is a page cache.** Performance is mostly about whether the pages you touch are already in
   RAM. "The working set fits in memory" is the single most important sentence in database tuning.

Both engines also use **write-ahead logging**: a commit appends the change to a sequential log and
flushes *that* to disk. The modified data pages are written later, in the background, at checkpoints.
Sequential log writes are cheap; random page writes are batched. That is how a database can commit
thousands of transactions per second on ordinary disks.

## B-trees in one minute

Both engines index data with **B+trees**: shallow, wide trees where each node is one page. A page of
16 KB can hold hundreds of keys, so a tree only three or four levels deep can address billions of
rows. Looking up a key means reading one page per level — and the top levels are almost always cached.

```text
                      [ root:  100 | 200 ]
                     /         |          \
        [ 10 | 40 | 70 ]  [ 120 | 160 ]  [ 230 | 260 | 290 ]     <- internal pages
          /   |   \  ...
   [1..9][10..39][40..69] ...                                    <- leaf pages, linked
```

Leaves are kept in key order and linked to each other, which makes **range scans** (`BETWEEN`,
`ORDER BY ... LIMIT`) efficient. When a leaf is full and a new key must go in the middle, the page
**splits** into two half-full pages. Remember page splits; they come back later.

Now the important part: *what is stored in the leaves?* This is where MySQL and PostgreSQL diverge.

## MySQL InnoDB: the table *is* the primary key index

InnoDB stores every table as a **clustered index**: a B+tree ordered by primary key whose leaf pages
contain **the complete rows**. There is no separate "table" — the primary key index is the table.

```text
  InnoDB clustered index (ordered by PRIMARY KEY)

  leaf page 7                    leaf page 8
  +---------------------------+  +---------------------------+
  | id=1  name=Ann  city=Oslo |  | id=4  name=Dan  city=Rome |
  | id=2  name=Bob  city=Lima |->| id=5  name=Eve  city=Kyiv |
  | id=3  name=Cid  city=Pune |  | id=6  ...                 |
  +---------------------------+  +---------------------------+
```

If you do not declare a primary key, InnoDB uses the first `UNIQUE NOT NULL` index, and if there is
none, it silently creates a hidden 6-byte row id to cluster on.

**Secondary indexes** (every index that is not the primary key) store the indexed columns plus the
**primary key value** — not a physical address:

```text
  secondary index on (city)            clustered index (the table)
  +----------------+                   +------------------------------+
  | Kyiv  -> id=5  |  --- lookup 2 --> | id=5 name=Eve city=Kyiv ...  |
  | Lima  -> id=2  |                   +------------------------------+
  | Oslo  -> id=1  |
  +----------------+
```

So `SELECT * FROM users WHERE city = 'Kyiv'` walks **two** B-trees: the secondary index to find
`id=5`, then the clustered index to fetch the row.

### MVCC in InnoDB: update in place, keep history in undo logs

To let readers see a consistent snapshot while writers change data (MVCC — multi-version concurrency
control), InnoDB **updates the row in place** in the clustered index and writes the *previous* version
into an **undo log**. Each row carries hidden columns: the id of the transaction that last changed it
and a pointer into the undo log. A reader that needs an older version follows that pointer and
reconstructs it. A background **purge** thread deletes undo records once no open transaction can need
them.

## PostgreSQL: a heap of row versions, and indexes that point into it

PostgreSQL stores a table as a **heap**: pages of rows in no particular order. New rows go wherever
there is free space — usually the end. Every index, *including the primary key*, is a separate B-tree
whose leaves store the key and a **TID** (tuple id): the physical address `(page number, slot)`.

```text
  primary key index (id)        heap (unordered pages)
  +-------------------+         page 0                        page 1
  | 1 -> (0,1)        |------>  +-------------------------+   +-------------------------+
  | 2 -> (1,1)        |---+     | (0,1) id=1 Ann  Oslo    |   | (1,1) id=2 Bob  Lima    |
  | 3 -> (0,2)        |-+ +---> |                         |   |                         |
  +-------------------+ +-----> | (0,2) id=3 Cid  Pune    |   |                         |
                                +-------------------------+   +-------------------------+
  index on (city)
  | Lima -> (1,1) |   every index points straight at the physical row version
```

An index lookup is one B-tree walk plus one heap page read — no second tree.

### MVCC in PostgreSQL: every update writes a new row version

PostgreSQL does not update rows in place. An `UPDATE` **inserts a complete new version** of the row
(a new *tuple*) and marks the old one as expired. Each tuple records which transaction created it
(`xmin`) and which one expired it (`xmax`); a reader checks those against its snapshot to decide which
version it can see. Old versions stay in the table until **VACUUM** (normally autovacuum) removes the
ones no transaction can see anymore and makes the space reusable.

```text
  UPDATE users SET city = 'Rome' WHERE id = 1;

  page 0
  +-----------------------------------------+
  | (0,1) id=1 Ann Oslo  xmin=100 xmax=205  |  <- dead once tx 205 commits; VACUUM reclaims it
  | (0,2) id=3 Cid Pune  xmin=101           |
  | (0,3) id=1 Ann Rome  xmin=205           |  <- new version
  +-----------------------------------------+
```

Because the new version has a **new physical address**, every index on the table would need a new
entry pointing to it — even indexes on columns you did not change. PostgreSQL softens this with
**HOT (heap-only tuple) updates**: if *no indexed column changed* and the new version *fits on the
same page*, the old tuple just points to the new one inside the page and the indexes are left alone.

## Why it matters: six practical consequences

### 1. Primary key choice (the UUID question)

In InnoDB, rows are physically ordered by primary key. With an **auto-increment** or other
time-ordered key, every insert goes to the right-most leaf page: pages fill up completely, the "hot"
page stays in memory, and there are almost no page splits.

With a **random UUIDv4**, each insert lands on a random leaf somewhere in the tree. Pages split
constantly and end up around half full, the whole table has to be in memory to keep inserts fast,
and inserting becomes random I/O once it is not. And since **every secondary index stores the primary
key**, a 16-byte (or, stored as text, 36-byte) key makes *all* indexes larger than an 8-byte integer
would.

In PostgreSQL the *heap* does not care about your primary key — new rows go at the end either way.
But the primary key **index** is still a B-tree, so random UUIDs still cause random inserts, page
splits and poor cache locality in that index (and extra WAL volume). The effect is smaller than in
InnoDB, but real on large, write-heavy tables.

> [!TIP]
> If you need globally unique ids (generated by clients, merged across databases, not guessable),
> use a **time-ordered** format such as **UUIDv7** or ULID. They are unique like UUIDv4 but sort by
> creation time, so inserts behave like auto-increment. PostgreSQL 18 added a built-in `uuidv7()`
> function; for older versions and for MySQL, generate them in the application.

### 2. Range scans and "rows that belong together"

In InnoDB you can choose a primary key that clusters related rows. With `PRIMARY KEY (user_id, id)`
on a `messages` table, all of one user's messages sit next to each other, so "latest 50 messages of
user 42" reads one or two pages.

In PostgreSQL an index range scan finds the 50 TIDs quickly, but the rows themselves may be scattered
across 50 different heap pages (each written whenever that message arrived). Mitigations:

- **Covering indexes** — `CREATE INDEX ... ON messages (user_id, id) INCLUDE (body)` lets an
  **index-only scan** answer the query without touching the heap at all (it needs the table's
  visibility map to be up to date, which VACUUM maintains).
- `CLUSTER` physically reorders a table by an index — once. New rows are not kept in order.
- **BRIN indexes** are tiny and very effective when data is *naturally* ordered on disk, such as an
  append-only `created_at` column.

### 3. Secondary index lookups

InnoDB: two B-tree walks per row (secondary, then clustered), but the clustered index is usually hot
in memory. PostgreSQL: one B-tree walk plus a heap fetch, plus a visibility check. Neither is strictly
faster; what matters is how many *distinct pages* a query touches. A query returning many rows by a
secondary index can be dramatically slower than one returning rows that are physically together.

### 4. Updates on tables with many indexes

This is the famous difference. In PostgreSQL, a non-HOT update must add an entry to **every index**
on the table, because the row moved. A table with 10 indexes and a frequently updated
`last_seen_at` column can generate ten index writes (and their WAL) per update. In InnoDB, updating a
non-indexed column changes the row in place and touches no secondary index at all; updating an indexed
column only touches the indexes that contain it.

Uber's 2016 post *"Why Uber Engineering Switched from Postgres to MySQL"* cited this **write
amplification** (and its effect on replication traffic) as a key reason for their move. PostgreSQL has
improved since — B-tree deduplication (v13) and bottom-up index deletion (v14) reduce index bloat from
version churn — but the basic model is unchanged.

What to do in PostgreSQL:

- Don't index columns that change often unless queries truly need them.
- Leave free space for HOT updates on update-heavy tables:
  `ALTER TABLE sessions SET (fillfactor = 80);`
- Watch HOT ratio: `n_tup_hot_upd` vs `n_tup_upd` in `pg_stat_user_tables`.
- Move hot, frequently-updated columns (counters, timestamps) into a narrow separate table.

### 5. Cleaning up old versions: VACUUM vs purge

Both engines must eventually discard old row versions, and both suffer when they cannot.

- **PostgreSQL**: dead tuples accumulate in the table and indexes until VACUUM removes them. If
  autovacuum is too slow for your write rate, tables and indexes **bloat** (grow far larger than the
  live data), and every scan reads the garbage too. VACUUM also "freezes" old rows to prevent
  32-bit transaction-id wraparound, so it is not optional.
- **InnoDB**: old versions live in undo logs; if purge falls behind, the **history list length**
  grows and reads that must reconstruct old versions get slower.

In both, the classic culprit is the same: a **long-running transaction** (or a forgotten open
transaction in a console) forces the database to keep every version created since it started.

```sql
-- PostgreSQL: who is holding back cleanup?
SELECT pid, state, xact_start, now() - xact_start AS age, left(query, 60)
FROM pg_stat_activity
WHERE xact_start IS NOT NULL
ORDER BY xact_start
LIMIT 5;
```

### 6. Big values

Rows must fit in a page, so both engines move large values out of line: PostgreSQL compresses and
stores large values (roughly over 2 KB) in a separate **TOAST** table; InnoDB stores long `TEXT`/`BLOB`
columns on overflow pages. Either way, `SELECT *` on a table with big columns reads those extra
pages. Select only the columns you need.

## Cheat sheet

| Situation | InnoDB | PostgreSQL |
|---|---|---|
| Random UUIDv4 primary key | Expensive: clustered table + all indexes affected | Hurts the PK index only |
| Insert-heavy, append-only | Great with sequential PK | Great |
| Update one column, many indexes | Only touches indexes on that column | Touches every index unless HOT |
| Range scan by primary key | Rows physically together | Index-ordered, heap may be scattered |
| Lookup by secondary index | Two B-tree walks | One B-tree walk + heap fetch |
| Long-running transactions | Undo history grows | Dead tuples pile up, bloat |
| Cleanup mechanism | Purge thread | (Auto)VACUUM |

## Key takeaways

- Databases read **pages**, not rows. Performance is about how many pages you touch and whether they
  are in memory.
- **InnoDB** = the table is a B-tree clustered by primary key; secondary indexes point to the primary
  key. Choose small, ever-increasing primary keys.
- **PostgreSQL** = the table is an unordered heap; all indexes point to physical row versions; updates
  create new versions that VACUUM must clean up. Index thoughtfully on update-heavy tables.
- Neither design is "better". They make different workloads cheap. Knowing which is which lets you
  design schemas that fit your database instead of fighting it.

## Further reading

- PostgreSQL docs: [Database Physical Storage](https://www.postgresql.org/docs/current/storage.html)
  and [Routine Vacuuming](https://www.postgresql.org/docs/current/routine-vacuuming.html)
- PostgreSQL docs: [Heap-Only Tuples (HOT)](https://www.postgresql.org/docs/current/storage-hot.html)
- MySQL docs: [Clustered and Secondary Indexes](https://dev.mysql.com/doc/refman/8.0/en/innodb-index-types.html)
  and [InnoDB Multi-Versioning](https://dev.mysql.com/doc/refman/8.0/en/innodb-multi-versioning.html)
- Uber Engineering (2016): [Why Uber Engineering Switched from Postgres to MySQL](https://www.uber.com/blog/postgres-to-mysql-migration/)
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapter 3 (Storage and Retrieval)
