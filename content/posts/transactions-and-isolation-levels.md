+++
title = "Transactions and isolation levels: what ACID really promises"
summary = "Atomicity, consistency, isolation and durability in plain words; the anomalies each isolation level allows (lost updates, write skew, phantoms); how PostgreSQL and MySQL differ; and practical patterns like SELECT FOR UPDATE and optimistic locking."
tags = ["database", "postgresql", "mysql", "backend"]
level = "intermediate"
date = 2026-10-02
+++

Two requests try to book the last seat on a flight at the same moment. Both read "1 seat left", both
insert a booking, and the flight is oversold. Wrapping the code in `BEGIN ... COMMIT` doesn't
necessarily prevent this. Understanding why — and what does — is the topic of this article.

## ACID in plain words

- **Atomicity** — all of a transaction's changes happen, or none do. If you crash after debiting one
  account and before crediting another, the debit is rolled back.
- **Consistency** — the database moves from one valid state to another: constraints (`NOT NULL`,
  foreign keys, `UNIQUE`, `CHECK`) are never violated by a committed transaction. (Your *business*
  rules are your job — unless you encode them as constraints, which you should where possible.)
- **Isolation** — concurrent transactions don't interfere with each other… *to a degree you choose*.
  This is the subtle one.
- **Durability** — once committed, data survives crashes (it's in the write-ahead log on disk).

## Why isolation is a dial, not a switch

Perfect isolation — every transaction behaves as if it ran alone, one after another — is called
**serializable**. It is expensive: the database must detect or prevent every possible conflict.
So databases offer weaker levels that are faster but allow certain **anomalies**.

### The anomalies

- **Dirty read** — reading another transaction's uncommitted changes (which may be rolled back).
- **Non-repeatable read** — reading the same row twice in one transaction and getting different
  values, because another transaction committed in between.
- **Phantom read** — running the same query twice and getting a different *set* of rows, because
  another transaction inserted or deleted matching rows.
- **Lost update** — two transactions read a value, both modify it in application code, both write;
  one update silently overwrites the other.

  ```text
  T1: read balance = 100            T2: read balance = 100
  T1: write balance = 100 + 50       T2: write balance = 100 - 30
  final: 70  (the +50 is lost)
  ```

- **Write skew** — two transactions read the same data, make decisions based on it, and write to
  *different* rows, together violating a rule neither violated alone. The doctors on-call example:
  the rule is "at least one doctor on call"; two on-call doctors each check "is someone else on call?
  yes" and both go off call.

### The standard levels

| Level | Dirty read | Non-repeatable read | Phantom | Lost update / write skew |
|---|---|---|---|---|
| Read uncommitted | possible | possible | possible | possible |
| Read committed | prevented | possible | possible | possible |
| Repeatable read | prevented | prevented | possible in the standard* | depends on the database* |
| Serializable | prevented | prevented | prevented | prevented |

\* Real databases differ from the SQL standard's definitions, which is why you need to know your
database's actual behaviour.

## PostgreSQL vs MySQL defaults

- **PostgreSQL defaults to Read Committed.** Each *statement* sees a snapshot of data committed before
  it began. Its Repeatable Read is snapshot isolation (no phantoms, and concurrent updates of the same
  row make the later transaction fail with a serialization error rather than silently losing an
  update). Its **Serializable** level uses Serializable Snapshot Isolation (SSI), which also detects
  write skew and aborts one of the transactions.
- **MySQL InnoDB defaults to Repeatable Read.** Plain `SELECT`s read from a consistent snapshot taken at
  the first read. But **locking reads and writes** (`SELECT ... FOR UPDATE`, `UPDATE`, `DELETE`) operate
  on the *latest committed* data and take locks, including **gap/next-key locks** that block inserts
  into scanned ranges. The mix of snapshot reads and current-data writes surprises many people: you can
  read a value from your snapshot, then update a row that has since changed.

In both, at Read Committed and in MySQL's Repeatable Read, **the lost update and write skew anomalies
are possible** if you read, decide in code, then write.

## Patterns that actually prevent the seat oversell

### 1. Do it in one atomic statement

Let the database check and change in one step:

```sql
UPDATE flights SET seats_left = seats_left - 1
WHERE id = 7 AND seats_left > 0;
-- 1 row updated: booked.  0 rows: sold out.
```

Single-statement updates like `SET x = x + 1` are safe at any level: the row is locked while updated,
and the condition is re-checked against the latest version.

### 2. Constraints

Let the database enforce invariants: `CHECK (seats_left >= 0)`, `UNIQUE (flight_id, seat_number)`,
exclusion constraints for overlapping bookings (PostgreSQL). Concurrent violators get an error instead
of corrupt data.

### 3. Pessimistic locking: `SELECT ... FOR UPDATE`

Lock the rows you are about to base a decision on:

```sql
BEGIN;
SELECT seats_left FROM flights WHERE id = 7 FOR UPDATE;   -- other transactions wait here
-- application checks seats_left > 0, computes price, etc.
INSERT INTO bookings (...) VALUES (...);
UPDATE flights SET seats_left = seats_left - 1 WHERE id = 7;
COMMIT;
```

Simple and correct, but transactions on the same row are serialised, and holding locks while doing slow
work (calling an API!) inside the transaction kills throughput. Keep such transactions short. Lock rows
in a consistent order to avoid deadlocks. When the thing to lock isn't a row ("a user's votes in a
poll"), an **advisory lock** works — this site uses one to enforce per-IP vote limits.

### 4. Optimistic locking: version columns

Don't lock; detect conflicts at write time:

```sql
-- read: SELECT id, title, body, version FROM documents WHERE id = 5;   -> version = 12
UPDATE documents SET body = $1, version = version + 1
WHERE id = 5 AND version = 12;
-- 0 rows updated => someone else saved first: reload and retry, or show a conflict to the user
```

Great for user-facing edits where conflicts are rare and transactions span human think-time (you can't
hold a lock while a user edits a form). ORMs often support this natively (`@Version`, `lock_version`).

### 5. Serializable isolation + retries

Run the transaction at `SERIALIZABLE` and let the database detect conflicts. Conflicting transactions
fail with a serialization error (SQLSTATE `40001`), and **your code must retry them**. Correct for
complex invariants like write skew, at some cost in throughput and with the obligation to handle
retries everywhere.

## Practical guidance

- Keep transactions **short**. Never wait for network calls, user input or large computations inside
  one.
- Prefer **atomic statements and constraints**; reach for explicit locks when you must read-then-write.
- Know your default level and which anomalies it allows.
- Handle **deadlock** (`40P01` in Postgres) and **serialization** (`40001`) errors with a retry — they
  are expected under concurrency, not bugs.
- Long-running transactions hurt everyone: they hold locks and prevent cleanup of old row versions
  (VACUUM in PostgreSQL, purge in InnoDB).
- Test concurrency explicitly: fire parallel requests at the critical endpoint and check the
  invariant. (This repository's tests include one that fires simultaneous votes to verify a limit holds.)

## Further reading

- PostgreSQL docs: [Transaction Isolation](https://www.postgresql.org/docs/current/transaction-iso.html) and [Explicit Locking](https://www.postgresql.org/docs/current/explicit-locking.html)
- MySQL docs: [Transaction Isolation Levels](https://dev.mysql.com/doc/refman/8.0/en/innodb-transaction-isolation-levels.html) and [InnoDB Locking](https://dev.mysql.com/doc/refman/8.0/en/innodb-locking.html)
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapter 7 (Transactions)
- [Jepsen consistency models](https://jepsen.io/consistency) — a map of isolation and consistency levels
