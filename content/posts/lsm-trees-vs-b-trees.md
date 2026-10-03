+++
title = "LSM trees vs B-trees: how storage engines trade reads for writes"
summary = "Why B-trees update pages in place and LSM trees never do: memtables, SSTables, compaction, bloom filters and tombstones, the read/write/space amplification trade-off, and when RocksDB, Cassandra or MyRocks beat InnoDB and PostgreSQL."
tags = ["database","nosql","performance"]
level = "advanced"
date = 2026-10-02
+++

Your `events` table in MySQL receives tens of thousands of small inserts per second. The primary key
is a random UUID, and the table is now much larger than RAM. Inserts have become slow and
unpredictable, and the disks are busy with small random writes. A colleague says: "Use Cassandra or
RocksDB. They are *write-optimized*." What does that mean, and what do you pay for it?

The answer is the storage engine's core data structure. Most databases use one of two families: the
**B-tree**, which updates data in place, and the **LSM tree** (log-structured merge tree), which
never does.

## The problem: small random writes into a big sorted structure

A recap from [how databases store data](/posts/how-databases-store-data): InnoDB and PostgreSQL
keep tables and indexes in B-trees made of fixed-size **pages** (16 KB in InnoDB, 8 KB in
PostgreSQL). To change one row, the engine appends the change to the **write-ahead log** (WAL),
loads the row's page into memory if needed, changes it there, and later writes the whole page back
to its place on disk.

This is **update in place**: each row lives in one place, which is overwritten. Reads love it: a
lookup walks one shallow tree to the one current copy of the row. Writes pay twice:

- **Read before write.** To insert a key, the engine must first find its page. If the page is not in
  memory, the insert waits for a random disk read. With random keys (like UUIDv4) and a table much
  larger than RAM, almost every insert does this.
- **Page-sized writes for row-sized changes.** Change a 100-byte row and the engine eventually writes
  a 16 KB page. If no other row on that page changed before the flush, the disk wrote about 160 times
  more bytes than you changed. Safety features add more: InnoDB's **doublewrite buffer** writes each
  page twice to survive a crash mid-write, and PostgreSQL copies a full page into the WAL the first
  time it changes after each checkpoint (the `full_page_writes` setting, on by default).

With sequential keys or a small working set, many changes share one page flush and this cost mostly
disappears. With random writes across a large table, it dominates.

## The LSM idea: never update in place

The LSM tree was described by O'Neil and colleagues in a 1996 paper, and made popular by Google's
Bigtable paper (2006) and Google's open-source LevelDB library. The rule: **never modify data on
disk. Write new sorted files, and merge them in the background.**

1. **WAL.** Append the change to a log file, as a B-tree engine does. It is read back only to
   rebuild the memtable after a crash or restart.
2. **Memtable.** Insert the key and value into an in-memory sorted structure (often a *skip list*).
   The write is now complete: no disk read, no page to find.
3. **Flush.** When the memtable is full (64 MB by default in RocksDB), it is frozen, a new one takes
   writes, and the frozen one is written as an **SSTable** (sorted string table): an immutable file
   of key-value pairs sorted by key. This is one large sequential write.
4. **Compaction.** A background process merge-sorts several SSTables into new files, keeps only the
   newest version of each key, and deletes the old files.

```text
 put(k, v)
    |----------------------------------+
    | 1. append                        | 2. insert (the write is done here)
    v                                  v
 [ WAL file ]                  [ memtable: sorted, in RAM ]
 (read only on recovery)               |
                                       | 3. when full: write it out as one sorted file
                                       v
 L0   [sst 9] [sst 8] [sst 7]          newest files; their key ranges overlap
          \      |      /
           \     |     /               4. compaction: merge-sort, keep newest version
 L1   [ a..f ][ g..m ][ n..z ]                  one sorted run: no overlaps
 L2   [a..c][d..f][g..i][j..m][n..q][r..z]      one sorted run, about 10x bigger than L1
```

An update is just a new write with the same key; compaction later discards the older version.

### Reading from an LSM tree

The price is paid on reads. A key may be in the memtable, any L0 file or any deeper level, so a
point lookup checks them **from newest to oldest** and stops at the first match:

```python
TOMBSTONE = object()   # marker written by delete(), see the next section

def get(key):
    for source in [memtable, *frozen_memtables, *sstables_newest_first]:
        if not source.might_contain(key):   # bloom filter says "definitely not here"
            continue
        value = source.lookup(key)
        if value is not None:
            return None if value is TOMBSTONE else value
    return None
```

In the leveled layout drawn above, files below L0 never overlap within one level, so at most one
file per level can hold the key. Each SSTable also has a small index and a bloom filter, usually
cached in memory, so a file that does not hold the key can usually be skipped without a disk read.
A **range scan** is harder: it must open a cursor on every memtable and file that
overlaps the range and merge them, like merging several sorted lists.

## Deletes are writes too: tombstones

SSTables are immutable, so a delete writes a **tombstone**: a marker saying "key k was deleted". A
read that meets the tombstone first returns "not found". The tombstone and the old value disappear
only when compaction merges them, and the tombstone can be dropped only when no older file could
still contain the key.

- **Deleting does not free space right away.** It even uses a little more until compaction runs.
- **Many tombstones make reads slow.** In a table used as a queue, reading "the oldest job" must
  step over thousands of deleted jobs. Cassandra protects itself: by default it logs a warning when
  one query scans more than 1,000 tombstones, and aborts the query above 100,000
  (`tombstone_warn_threshold` and `tombstone_failure_threshold` in `cassandra.yaml`).
- **Replicas must keep tombstones for a while.** In Cassandra, if a replica missed a delete and the
  tombstone is purged too early, a repair can copy the old value back: deleted data returns. So each
  table has `gc_grace_seconds` (864,000 seconds, or 10 days, by default): tombstones are kept at
  least that long, and every replica must be repaired more often than that.

Discord has described tombstones as one surprise of storing messages in Cassandra (see the
[Discord case study](/posts/discord-message-storage-case-study)).

## Compaction strategies: size-tiered vs leveled

Compaction decides which files are merged and when; most LSM tuning happens here. A **sorted run**
is a set of files that together hold one sorted, non-overlapping sequence of keys.

- **Size-tiered** (the default in Cassandra's standard `cassandra.yaml`, similar to RocksDB's
  "universal" compaction): when there are several files of similar size (four by default in
  Cassandra), merge them into one bigger file.
- **Leveled** (LevelDB's design, RocksDB's default, available in Cassandra and ScyllaDB): each level
  below L0 is one sorted run of small files. From L1 down, each level is about 10 times larger than
  the one above it. When a level outgrows its target, one file is merged with only the overlapping
  files in the next level.

```text
 Size-tiered:  [1] [1] [1] [1]  ->  [  4  ]      [4] [4] [4] [4]  ->  [      16      ]
               few rewrites per byte, but one key can exist in many files

 Leveled:  L1  [a-f]          [g-m]          [n-z]
                                |  merge one file with the overlapping files below
                                v
           L2  [a-c][d-f]  [g-h][i-k][l-m]  [n-q][r-z]
               a key can be in at most one file per level
```

| | Size-tiered | Leveled |
|---|---|---|
| Write amplification | Lower: each byte is rewritten about once per size tier | Higher: up to ~10 rewrites per level (the size ratio) in the worst case |
| Read amplification | Higher: a key may be in many files | Lower: at most one file per level |
| Space amplification | Higher: old versions live longer; a big merge needs free space as large as its inputs | Lower: roughly 10% extra in steady state (with a ratio of 10, all upper levels together are about 1/9 of the last level) |
| Good for | Write-heavy data, rarely updated or read | Read-heavy or update-heavy data |

**Time-window** compaction (TWCS in Cassandra and ScyllaDB) groups data by write time, for example
one file per day. For time series with a TTL (time to live), a whole file expires and is simply
deleted. This works well only when rows arrive roughly in time order and are not updated or deleted
later. Newer designs blend the families, such as the Unified Compaction Strategy (UCS) added in
Cassandra 5.0 and ScyllaDB's incremental compaction.

## Bloom filters: what makes LSM reads practical

A **bloom filter** is a small bit array plus a few hash functions, built for each SSTable. Ask it
"could key k be in this file?" and it answers **"definitely not"** or **"maybe"**. "Definitely not"
lets a read skip the file without touching the disk.

About 10 bits per key gives roughly a 1% false-positive rate (wrong "maybe" answers): a billion keys
need about 1.25 GB of filter memory. Bloom filters help **point lookups**, especially for keys that
do not exist. They do not help ordinary range scans (RocksDB can build filters on key *prefixes* for
scans inside one prefix).

## Read, write and space amplification

Engineers compare storage engines with three ratios:

- **Write amplification:** bytes written to disk ÷ bytes the application wrote (disk bandwidth,
  SSD wear).
- **Read amplification:** disk reads per logical read (read latency).
- **Space amplification:** bytes on disk ÷ bytes of live data (disk cost).

A 2016 research paper called this the *RUM conjecture*: you can optimise at most two of **R**ead
cost, **U**pdate cost and **M**emory (space) overhead, at the expense of the third.

| | B-tree (InnoDB, PostgreSQL) | LSM, leveled | LSM, size-tiered |
|---|---|---|---|
| Write amp | High for small random writes | Medium | Low |
| Point lookup | One tree walk | Low with bloom filters | Medium |
| Range scan | Cheap: leaves are linked in order | Merge across levels | Merge across many files |
| Space amp | Medium: partly empty pages, fragmentation, bloat | Low | High |
| Compression | Limited: InnoDB pads compressed pages to fixed sizes; PostgreSQL compresses only large values | Good: files written once | Good |

One cost hides in the LSM columns: **compaction uses disk bandwidth and CPU all the time**. If writes
outpace it, files pile up and reads slow down. RocksDB then slows and finally stops incoming writes
(a *write stall*) until compaction catches up. B-tree read latency is more predictable.

## Who uses what

| System | Structure | Notes |
|---|---|---|
| MySQL InnoDB | B+tree | Table clustered by primary key; old versions in undo logs |
| PostgreSQL | Heap + B-tree indexes | New row version per update; VACUUM cleans up |
| LevelDB | LSM, leveled | Google's embeddable key-value library |
| RocksDB | LSM, leveled by default | Started as a LevelDB fork at Facebook; embedded in Kafka Streams, TiKV, MyRocks and more |
| Apache Cassandra | LSM | Compaction chosen per table: STCS (default), LCS, TWCS, and UCS (5.0 and later) |
| ScyllaDB | LSM | Cassandra-compatible, written in C++ |
| Apache HBase | LSM on HDFS | MemStore + HFiles + WAL; minor and major compactions |

## MyRocks: an LSM engine inside MySQL

You do not have to leave SQL to get an LSM tree. MySQL lets you choose the storage engine per table,
and **MyRocks**, created at Facebook, is a storage engine built on RocksDB. You keep MySQL's SQL,
replication and tools; only the on-disk structure changes. It was developed in Facebook's own MySQL
fork (whose public GitHub repository was archived in 2026), and it is also available in Percona
Server for MySQL and MariaDB.

```sql
CREATE TABLE events (
  id         BIGINT UNSIGNED NOT NULL PRIMARY KEY,
  user_id    BIGINT UNSIGNED NOT NULL,
  created_at DATETIME NOT NULL,
  body       TEXT,
  KEY by_user (user_id, created_at)
) ENGINE=ROCKSDB;
```

Facebook has described moving its main MySQL database, the User Database (UDB), from InnoDB to
MyRocks, mainly to use less storage space and cause less write amplification (see the VLDB paper
below). The project's wiki also reports an older LinkBench (social-graph benchmark) run where MyRocks
used about half the disk space of InnoDB and wrote less than half as many bytes per second. It is an
old benchmark, and results depend heavily on settings such as InnoDB compression, so treat it as one
data point, not a promise for your workload.

The same wiki lists missing features, including foreign keys, full-text and spatial indexes, and gap
(next-key) locks; because of the missing gap locks, replication must use row-based binary logging.
Percona and MariaDB keep their own limitation lists, so check the one for the build you run. MyRocks
fits large, write-heavy MySQL datasets where storage is the bottleneck, not databases that fit in
RAM.

## When each fits

**Stay with a B-tree engine** (the right default for most applications) when reads dominate, you
need predictable read latency, you rely on range scans, joins, secondary indexes and ad-hoc queries,
or the working set mostly fits in memory, which keeps page flushes batched and cheap.

**Consider an LSM engine** when:

- writes dominate and arrive in random key order: events, messages, metrics, logs, sensor data;
- the dataset is much larger than RAM, and disk space or SSD wear is a real cost;
- writes are "blind" (no read first), so an upsert is just a new version;
- data expires by time, so whole files can be dropped.

**Be careful with LSM** for delete-heavy or queue-like workloads (tombstones), read-modify-write
patterns (every write then needs a read first, so the LSM write advantage shrinks), and strict
read-latency targets under heavy writes.

> [!TIP]
> Don't switch engines because of a reputation. First try the cheap fixes for B-tree write pain:
> time-ordered keys such as UUIDv7, fewer indexes on write-heavy tables (see
> [database indexes](/posts/database-indexes-and-explain)), batched inserts, partitioning by time.

## In practice: a checklist

- [ ] Measure real write amplification: bytes the disk writes (`iostat`, cloud metrics) ÷ bytes
      your application writes.
- [ ] On an LSM store, watch pending compactions and files touched per read (Cassandra:
      `nodetool compactionstats`, `nodetool tablehistograms`; RocksDB: compaction stats in its LOG).
- [ ] With size-tiered compaction, keep plenty of free disk for big merges.
- [ ] Choose compaction per table: time-window for TTL'd time series, leveled for read- or
      update-heavy tables, size-tiered for write-mostly ones.
- [ ] Benchmark with more data than RAM, long enough for compaction to reach a steady state.

## Common mistakes

- **"LSM is faster."** It is faster for some writes. Reads, especially range scans, usually cost more.
- **Using an LSM store as a queue.** Tombstones collect at its head. Use a real
  [message queue](/posts/message-queues-and-event-streams).
- **Expecting a delete to free disk space now.** Space returns only after compaction (in Cassandra,
  the tombstones themselves stay for at least `gc_grace_seconds`).
- **Running disks nearly full.** Without headroom, compaction stalls and reads degrade.
- **Benchmarking for five minutes.** Compaction debt appears hours later.
- **Blaming the B-tree for a key or index problem.** Random UUIDv4 keys and ten indexes on an
  insert-heavy table hurt any engine.

## Further reading

- O'Neil, Cheng, Gawlick and O'Neil, *The Log-Structured Merge-Tree (LSM-Tree)*, Acta Informatica,
  1996
- LevelDB: [implementation notes](https://github.com/google/leveldb/blob/main/doc/impl.md), a short
  description of a leveled LSM tree
- RocksDB wiki: [Compaction](https://github.com/facebook/rocksdb/wiki/Compaction),
  [Leveled Compaction](https://github.com/facebook/rocksdb/wiki/Leveled-Compaction) and
  [RocksDB Bloom Filter](https://github.com/facebook/rocksdb/wiki/RocksDB-Bloom-Filter)
- MyRocks wiki (in an archived repository): [advantages over InnoDB](https://github.com/facebook/mysql-5.6/wiki/MyRocks-advantages-over-InnoDB)
  and [limitations](https://github.com/facebook/mysql-5.6/wiki/MyRocks-limitations)
- Matsunobu, Dong and Lee, [*MyRocks: LSM-Tree Database Storage Engine Serving Facebook's Social
  Graph*](https://doi.org/10.14778/3415478.3415546), PVLDB vol. 13, 2020
- Athanassoulis et al., *Designing Access Methods: The RUM Conjecture*, EDBT 2016
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapter "Storage and Retrieval"
  (chapter 3 in the first edition); Alex Petrov, *Database Internals*, part I
