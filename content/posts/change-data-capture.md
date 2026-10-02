+++
title = "Change data capture (CDC): streaming every database change"
summary = "How to turn every insert, update and delete in PostgreSQL or MySQL into a stream of events with logical decoding, the binlog and Debezium, what CDC is good for, and the pitfalls: stuck replication slots, snapshots, schema changes and duplicates."
tags = ["database","messaging","distributed-systems"]
level = "intermediate"
date = 2026-10-02
+++

Your shop stores products in PostgreSQL and also in Elasticsearch, so customers can search them.
The code that saves a product also updates the search index. One day the index update times out
after the database commit succeeded, and search shows the old price. Later someone fixes prices
with a manual `UPDATE` in a SQL console, and search never hears about it. You add a nightly job
that rebuilds the index, and search is now "correct by tomorrow".

**Change data capture (CDC)** is a better answer: read every committed change directly from the
database's own log and publish it as an event. Every copy of the data can then follow along, no
matter which code path (or which human) made the change.

## The problem: many copies of the same data

Real systems keep the same data in several places: a search index, a cache, a data warehouse and
read copies inside other services. Syncing them from application code means writing to two
systems "at the same time". That is the **dual-write problem**: one write can succeed while the
other fails, and no ordering of the two calls is safe (see
[distributed transactions and the outbox](/posts/distributed-transactions-saga-outbox)).

What we want instead:

- every **committed** change, including deletes,
- in the **order** the database applied them,
- with no changes to every place in the code that writes data.

## Three ways to capture changes

**1. Polling.** A job runs a query every few seconds:

```sql
SELECT * FROM products
WHERE updated_at > :last_seen
ORDER BY updated_at
LIMIT 1000;
```

It is simple, but it misses hard deletes (the row is gone), misses intermediate states between
two polls, and depends on every write setting `updated_at`. There is also a subtle bug: a
transaction can set `updated_at` at 10:00:00 but commit at 10:00:05. If the poller ran at
10:00:03 and moved `last_seen` past 10:00:00, that row is skipped forever.

**2. Triggers.** A database trigger copies each insert, update and delete into a `changes` table,
inside the same transaction. This captures deletes and every change. But each write now costs an
extra write, triggers must follow every schema change, and something still polls the `changes`
table, with the same commit-order problem: a sequence ID is assigned at insert, not at commit.

**3. Log-based CDC.** Most databases already write a log of changes. In PostgreSQL this is the
**WAL** (write-ahead log), used for crash recovery and for replication. In MySQL it is the
**binlog** (binary log), used for replication and point-in-time recovery.
A CDC tool reads this log the same way a replica does (see
[replication](/posts/replication-and-high-availability)) and turns each row change into an event.

| | Polling | Triggers | Log-based CDC |
|---|---|---|---|
| Captures deletes | No (unless soft deletes) | Yes | Yes |
| Every intermediate change | No | Yes | Yes |
| Commit order | Approximate | Approximate | Exact |
| Extra load on writes | None | One extra write per change | Very little |
| Latency | Poll interval | Poll interval | Often under a second |
| Setup | Just SQL | SQL triggers | Database config, extra service |

Log-based CDC wins on correctness. Its cost is operational: a new component to run, and (as we
will see) a new way to hurt your primary database.

## How it works in PostgreSQL

PostgreSQL's WAL describes changes at the level of disk pages. **Logical decoding** turns those
records back into row changes ("row 42 in `orders` was updated, here are the new values"). Three
pieces matter:

- **`wal_level = logical`** in the server config. It adds the extra information decoding needs.
  Changing it requires a restart.
- A **publication** lists which tables to stream. Publications and the built-in `pgoutput`
  decoder plugin exist since PostgreSQL 10.
- A **replication slot** is a named bookmark on the server. It remembers how far one consumer has
  read, so the consumer can disconnect and later continue exactly where it stopped. The server
  keeps all WAL the slot still needs.

```sql
-- postgresql.conf: wal_level = logical   (then restart)

CREATE PUBLICATION shop_cdc FOR TABLE orders, products;

-- Debezium creates the slot itself if it does not exist (its database user
-- needs the REPLICATION privilege). To create it by hand instead:
SELECT pg_create_logical_replication_slot('shop_debezium', 'pgoutput');
```

Changes come out **only after commit**, one whole transaction at a time, in **commit order**.
That solves the polling bug above. Positions in the WAL are called **LSNs** (log sequence
numbers): they only grow, so they work as a natural "version" for each change.

## How it works in MySQL

MySQL writes the binlog for replication. For CDC it must be in **row format**: each event contains
the actual row values before and after the change, not the SQL statement that caused it. (A
statement like `UPDATE ... WHERE created_at < NOW()` can't tell a reader which rows changed.)

```ini
[mysqld]
server_id                  = 1
log_bin                    = mysql-bin
binlog_format              = ROW
binlog_row_image           = FULL      # include all columns, not only changed ones
gtid_mode                  = ON        # global transaction IDs: easier to resume after failover
enforce_gtid_consistency   = ON
binlog_expire_logs_seconds = 604800    # keep binlogs 7 days (MySQL 8 default: 30 days)
```

In MySQL 8, the binlog, `ROW` format and `FULL` row images are already the defaults; the file
above just makes them explicit. (Recent MySQL versions mark the `binlog_format` setting itself as
deprecated, so setting it may log a warning. `ROW` is still the default.)

A CDC connector connects to MySQL as if it were one more replica and reads the binlog. Note the
difference from PostgreSQL: **MySQL does not wait for a reader that is away**. It deletes old
binlog files after the expiry time. If your connector is down for longer than that, the position
it needs is gone, and it must start again from a full snapshot. Managed services can use much
shorter retention than self-hosted defaults, so check the setting.

## Debezium and Kafka Connect

You rarely write a log reader yourself. **Debezium** is a widely used open-source CDC tool, with
connectors for PostgreSQL, MySQL, SQL Server, MongoDB, Oracle and others. It usually runs inside
**Kafka Connect**, a part of Apache Kafka that runs "connectors": *source* connectors bring data
into Kafka, *sink* connectors copy data from Kafka into other systems. Kafka Connect is configured
through a REST API. In distributed mode it stores each connector's position (its **offset**) in a
Kafka topic, and moves tasks to another worker when a worker dies. But a task that fails with an
error stays `FAILED`: Kafka Connect does not restart it automatically, so alert on task status and
restart it through the REST API. Without Kafka, Debezium Server can send events to systems such as
Amazon Kinesis or Redis Streams.

```text
 application --writes--> [ PostgreSQL ]
                              |  WAL, decoded: committed changes only
                              v
                  replication slot "shop_debezium"  (bookmark: how far was read)
                              |
                              v
                  [ Debezium source connector ]      runs inside Kafka Connect
                              |  one event per row change, key = primary key
                              v
                  [ Kafka topic "shop.public.orders" ]
                     |                 |                  |
                     v                 v                  v
              search indexer    cache invalidator   warehouse sink connector
```

A connector is registered with a JSON config (credentials omitted here):

```json
{
  "name": "shop-cdc",
  "config": {
    "connector.class": "io.debezium.connector.postgresql.PostgresConnector",
    "plugin.name": "pgoutput",
    "database.hostname": "db.internal",
    "database.dbname": "shop",
    "database.user": "cdc_reader",
    "topic.prefix": "shop",
    "table.include.list": "public.orders,public.products",
    "publication.name": "shop_cdc",
    "slot.name": "shop_debezium",
    "heartbeat.interval.ms": "10000"
  }
}
```

Each row change becomes one event. Simplified, an update looks like this. (This full `before`
needs `REPLICA IDENTITY FULL`. By default PostgreSQL logs no old values except, at most, the
primary key; see *Deletes* below.)

```json
{
  "op": "u",
  "before": { "id": 1042, "status": "PENDING", "total_cents": 5990 },
  "after":  { "id": 1042, "status": "PAID",    "total_cents": 5990 },
  "source": { "db": "shop", "table": "orders", "lsn": 24023128 },
  "ts_ms": 1790000000000
}
```

`op` is `c` (create), `u` (update), `d` (delete) or `r` (read, a row from the initial snapshot).
The Kafka message key is the row's primary key (for tables that have one), so all changes to one
row land in the same partition and stay in order.

## What CDC is used for

- **Search indexes.** A consumer upserts each changed row into Elasticsearch or OpenSearch and
  deletes it on `d`. No more dual writes in the application.
- **Caches.** A consumer deletes (or refreshes) the cache key when a row changes. This catches
  changes made by scripts and other services too (see
  [caching strategies](/posts/caching-strategies)).
- **Analytics and data warehouses.** Instead of a heavy nightly export, changes flow continuously
  into object storage or a warehouse, usually through a sink connector.
- **Sharing data between services.** Another service keeps its own local read copy of, say,
  customers, and stays available when the customer service is down.
- **Relaying a transactional outbox.** The service writes business events into an `outbox` table
  in the same transaction as its data, and CDC publishes those rows. Debezium's "outbox event
  router" routes each row to a topic based on a column such as `aggregatetype` and uses the
  `payload` column as the message body. Because the connector reads inserts from the log, you can
  delete outbox rows right after inserting them, so the table never grows. (The router ignores
  the delete events.)
- **Database migrations.** Keep a new database in sync with the old one until you switch over.

## Pitfalls and common mistakes

### A stuck replication slot can fill your disk

This is the most important one. A PostgreSQL slot keeps **all** WAL from its position onwards. If
the connector is stopped, broken, or simply slow, WAL piles up on the primary's disk. When the
disk is full, PostgreSQL cannot write new WAL and stops. A forgotten test slot can take down
production weeks later. An old logical slot also stops `VACUUM` from cleaning up dead rows in the
system catalogs.

```text
 WAL on disk:  [seg][seg][seg][seg][seg][seg][seg][seg] ... [now]
                 ^
                 slot position: nothing after this point may be removed
```

Defences:

- **Monitor every slot** and alert on retained WAL and on `active = false`:

  ```sql
  -- wal_status exists in PostgreSQL 13 and later
  SELECT slot_name, active, wal_status,
         pg_size_pretty(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn)) AS retained_wal
  FROM pg_replication_slots;
  ```

- **Set `max_slot_wal_keep_size`** (PostgreSQL 13 and later). The default, `-1`, means no limit.
  With a limit, a slot that falls too far behind is invalidated (`wal_status = 'lost'`) instead of
  filling the disk. You must then drop the slot, create a new one and re-snapshot. That is painful
  but far better than an outage.
- **Drop slots you no longer use:** `SELECT pg_drop_replication_slot('old_slot');`
  PostgreSQL 18 also added `idle_replication_slot_timeout`, which invalidates slots that have
  not been used for longer than the given time (default `0`: never).
- **Use heartbeats.** A slot only moves forward when the connector confirms progress. If your
  captured tables are quiet but the rest of the database is busy, WAL grows even with a healthy
  connector. Set Debezium's `heartbeat.interval.ms` so it confirms progress regularly. If that is
  not enough (for example, when another database on the same server is the busy one), also set
  `heartbeat.action.query` to a statement that writes a row to a small heartbeat table. That
  table must be in the publication.
- **Plan for failover.** Before PostgreSQL 17, PostgreSQL itself did not copy logical slots to
  standby servers (some high-availability tools and extensions had their own workarounds).
  After a failover to a standby, the slot was gone, and the connector needed a new snapshot to be
  sure it missed nothing. PostgreSQL 17 can synchronise logical slots to standbys: the slot must
  be created with the `failover` option, and the standby needs `sync_replication_slots = on`
  plus a few related settings described in the PostgreSQL documentation. Recent Debezium versions
  have a `slot.failover` setting for this.

### The initial snapshot

The log only contains *recent* changes, not your whole history. So a new connector first takes a
**snapshot**: it records the current log position, reads every existing row (emitted as `op: r`),
and then streams changes from the recorded position. For big tables this takes hours, adds read
load, and on PostgreSQL holds one long transaction open, which stops `VACUUM` from cleaning up
dead rows. Debezium's **incremental snapshots** read tables in small, restartable chunks while
streaming continues. You start one by sending a *signal* to the connector (for example, by
inserting a row into a small signalling table). You can also use them to backfill tables you add
to the connector later.

### Schema changes

Adding a nullable column is usually harmless. Renaming or dropping a column, or changing a type,
breaks every consumer that reads it. Treat captured tables like an API: use expand-and-contract
migrations, and consider a **schema registry** (a service that stores event schemas and rejects
incompatible changes) with Avro, Protobuf or JSON Schema. For MySQL, Debezium also keeps a
**schema history** topic: a record of every DDL statement, so it can interpret old binlog events
with the table structure they had at that time. Never delete it or let Kafka expire its messages.

### Duplicates and ordering

Delivery is **at least once**. In normal operation each change arrives once, but the connector
saves its offset only periodically. If it crashes after publishing events but before saving the
offset, it publishes those events again after the restart. Consumers must be **idempotent** (safe
to apply twice). See [retries and idempotency](/posts/retries-timeouts-and-idempotency).

- Upserts by primary key are naturally idempotent.
- Store the event's LSN (or a row version) with the copy, and ignore events older than what you
  have. In PostgreSQL, a later change to the same row has a higher LSN. Elasticsearch supports
  this with external versioning (`version_type=external`). A replayed event then fails with a
  version conflict, which your consumer should treat as "already applied", not as an error.

Order is guaranteed **per key only**. Changes to different rows or tables may arrive in another
order, and one transaction becomes separate events: a consumer can see an order before its order
lines. If that matters, Debezium can publish transaction boundaries to a separate topic
(`provide.transaction.metadata`).

### Deletes and tombstones

A delete becomes a `d` event with `after: null`. By default PostgreSQL logs only the primary key of
the old row (the table's `REPLICA IDENTITY` is `DEFAULT`), so `before` holds just the key. The
`before` of an update then has no old values either, at most the primary key. If consumers need
the old values, for example the tenant ID to find the right cache key, set
`ALTER TABLE orders REPLICA IDENTITY FULL;` (this writes more WAL).

Watch out for tables **without a primary key**. Once such a table is in a publication that
publishes updates and deletes, PostgreSQL rejects `UPDATE` and `DELETE` on it with an error. Give
it a primary key, or set `REPLICA IDENTITY FULL` (or `USING INDEX` with a unique index that is
not partial and covers only `NOT NULL` columns).

After the delete event, Debezium also sends a **tombstone**: a message with the same key and a
`null` value. Kafka **log compaction** (keeping only the latest message per key) uses tombstones to
remove the key completely. Consumers must handle `null` values without crashing.

### Your tables become a public API

Table-level CDC exposes your internal schema to everyone downstream. Refactoring now needs a
meeting, and private columns (password hashes, personal data) flow into Kafka and the warehouse.

- Capture only what you need: `table.include.list`, Debezium's `column.exclude.list`, or
  PostgreSQL publication column lists (PostgreSQL 15 and later).
- For data shared with **other teams**, publish deliberately designed events through an outbox
  instead of raw rows. Keep raw CDC for copies you own, like your own search index.

## When not to use CDC

- **Small systems with one consumer.** A background job polling an outbox table (see
  [background jobs](/posts/background-jobs-and-cron)) is often enough and has fewer moving parts.
- **You need business events.** A row change says "status went from PENDING to PAID". It does not
  say *why*. Use an outbox for meaningful events, and CDC as the relay if you like.
- **You can't operate it.** CDC adds a broker, Kafka Connect, connectors and monitoring, and it
  puts a slot on your primary database. Managed services (for example AWS Database Migration
  Service) run the CDC process for you, but most pitfalls above still apply.

CDC is also not [event sourcing](/posts/event-sourcing-and-cqrs). In event sourcing the events
*are* the source of truth. With CDC the tables stay the truth, and events are derived from them.

## Checklist

- [ ] `wal_level = logical` (PostgreSQL) or `binlog_format = ROW` with enough retention (MySQL).
- [ ] Alerts on retained WAL per slot and on inactive slots; `max_slot_wal_keep_size` is set.
- [ ] Alerts on connector tasks in the `FAILED` state.
- [ ] Heartbeats configured for quiet tables.
- [ ] A plan for the initial snapshot and for re-snapshots after a lost slot or failover.
- [ ] Consumers are idempotent and handle deletes and `null` tombstones.
- [ ] Only needed tables and columns are captured; other teams get outbox events, not raw rows.
- [ ] Schema changes to captured tables are reviewed like API changes.

## Further reading

- PostgreSQL documentation: [Logical Decoding](https://www.postgresql.org/docs/current/logicaldecoding.html)
- PostgreSQL documentation: [Replication settings](https://www.postgresql.org/docs/current/runtime-config-replication.html), including `max_slot_wal_keep_size`
- PostgreSQL documentation: [Logical replication failover](https://www.postgresql.org/docs/current/logical-replication-failover.html) (PostgreSQL 17 and later)
- MySQL documentation: [Binary Logging Formats](https://dev.mysql.com/doc/refman/8.0/en/binary-log-formats.html)
- [Debezium documentation](https://debezium.io/documentation/), including incremental snapshots and the outbox event router
- Apache Kafka documentation: [Kafka Connect](https://kafka.apache.org/documentation/#connect)
- Martin Kleppmann, *Designing Data-Intensive Applications* (first edition), chapter 11 "Stream Processing", the part on change data capture in the section "Databases and Streams"
