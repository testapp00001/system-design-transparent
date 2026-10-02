+++
title = "Choosing a database: relational, key-value, document, wide-column, time-series, search, graph, vector"
summary = "A practical map of database types: what each is built for, real examples, and a simple decision process — including why 'start with PostgreSQL' is good advice and when it stops being enough."
tags = ["database", "nosql", "system-design"]
level = "beginner"
date = 2026-10-02
+++

There are hundreds of databases, and each one's marketing says it is the fastest. The useful
question is not "which database is best?" but **"what shape is my data, and how will I read and write
it?"** Different database families are optimised for different answers.

This article gives you a map. For each family: what it is, how it stores data, what it is great at,
what it is bad at, and well-known examples.

## The two questions that decide almost everything

1. **Access pattern.** How do you query the data? By primary key only? By arbitrary filters? Joins
   across entities? Full-text search? "Latest N items for X"? Aggregates over billions of rows?
2. **Consistency and correctness needs.** Do multiple records need to change together atomically
   (money, inventory)? Can readers tolerate slightly stale data?

Then come scale (data size, reads/s, writes/s), latency targets, and — often forgotten —
**operations**: who will run, back up, upgrade and debug this thing at 3 a.m.?

## Relational (SQL) databases

**Examples:** PostgreSQL, MySQL/MariaDB, SQL Server, Oracle, SQLite.

Data lives in tables with a schema; relationships are expressed with foreign keys and combined at
query time with joins. **ACID transactions** let you change many rows atomically. A query planner turns
declarative SQL into an efficient plan using indexes.

- **Great at:** business data with relationships (users, orders, payments), correctness, ad-hoc
  queries you didn't plan for, reporting, constraints that protect data quality.
- **Weaker at:** horizontal write scaling beyond one primary (possible but requires sharding),
  extremely high write rates of simple records, schemaless data that changes shape constantly.

Modern PostgreSQL also covers a lot of "NoSQL" ground: `JSONB` documents with indexes, full-text
search, geospatial (PostGIS), time-series (TimescaleDB extension), vectors (pgvector). That is why
"start with Postgres" is common advice.

## Key-value stores

**Examples:** Redis, Valkey, Memcached, etcd, Amazon DynamoDB (key-value + document), RocksDB
(embedded).

A giant hash map: `get(key)`, `set(key, value)`, sometimes with TTLs and data structures (lists,
sets, sorted sets in Redis). Lookups by key are extremely fast; anything else is hard.

- **Great at:** caching, sessions, rate-limit counters, leaderboards (Redis sorted sets), feature
  flags, distributed locks and configuration (etcd — the store behind Kubernetes).
- **Weaker at:** queries by anything other than the key, relationships, reporting.
- **Watch out:** many in-memory stores can lose recent writes on crash depending on persistence
  settings. Treat a cache as a cache.

## Document databases

**Examples:** MongoDB, Couchbase, Firestore, Amazon DocumentDB; PostgreSQL `JSONB`.

Records are JSON-like documents that can nest arrays and objects; documents in a collection need not
share a schema. You can index fields inside documents.

- **Great at:** data that is naturally a self-contained tree read as a whole (a product with its
  variants, a CMS page with blocks, a user profile with preferences); evolving schemas.
- **Weaker at:** data with many-to-many relationships (you end up duplicating data or doing joins in
  application code), multi-document transactions (supported in modern MongoDB, but not the design
  centre).
- **Rule of thumb:** if you keep writing `$lookup` (joins) or keeping duplicated data in sync, your data
  is relational.

## Wide-column stores

**Examples:** Apache Cassandra, ScyllaDB, HBase, Google Bigtable.

Data is partitioned across many nodes by a **partition key**; within a partition, rows are sorted by a
**clustering key**. Storage uses LSM trees (log-structured merge trees): writes go to memory and an
append-only log, then are flushed to immutable sorted files and merged in the background. Writes are
very cheap; reads may consult several files.

- **Great at:** massive write throughput, huge datasets spread over many machines, time-ordered data
  per entity ("all messages in channel X, newest first"), multi-datacenter replication.
- **Weaker at:** ad-hoc queries (you design one table per query), joins, transactions, aggregates.
  You must know your queries up front.
- **Real example:** Discord stores trillions of chat messages partitioned by channel and time bucket —
  see the [Discord case study](/posts/discord-message-storage-case-study).

## Time-series databases

**Examples:** TimescaleDB (on PostgreSQL), InfluxDB, Prometheus (metrics), VictoriaMetrics, QuestDB.

Optimised for append-only measurements with timestamps: metrics, sensor readings, prices, events.
They partition by time, compress aggressively (consecutive values are similar), downsample old data
and expire it automatically.

- **Great at:** "average CPU per host per minute for the last 7 days", retention policies, high ingest.
- **Weaker at:** updating old records, relational data.

## Analytical (columnar / OLAP) databases

**Examples:** ClickHouse, DuckDB (embedded), BigQuery, Snowflake, Amazon Redshift, Apache Druid.

They store data **by column** instead of by row. A query summing one column over a billion rows reads
only that column, compressed — orders of magnitude less I/O than a row store.

- **Great at:** analytics, dashboards, aggregations over huge datasets, event/log analysis.
- **Weaker at:** many small transactional updates, point lookups by key at high concurrency.
- **Typical setup:** your app writes to an OLTP database (Postgres/MySQL); data is copied (via CDC or
  batch ETL) into an OLAP store for analysis. Running heavy analytics on your production OLTP primary
  is a classic way to slow your app down.

## Search engines

**Examples:** Elasticsearch, OpenSearch, Apache Solr, Meilisearch, Typesense; PostgreSQL full-text
search for smaller needs.

Built around an **inverted index**: a map from each word to the documents containing it, plus
relevance scoring, stemming ("running" matches "run"), typo tolerance, facets and highlighting.

- **Great at:** search boxes, log search, faceted filtering ("brand: X, price < 100, in stock").
- **Weaker at:** being the source of truth (they are usually fed from your main database), strong
  consistency (new documents become searchable after a short refresh interval).
- **Tip:** PostgreSQL's built-in full-text search (`tsvector`, GIN indexes, `pg_trgm` for fuzzy
  matching) is enough for many sites — this one included.

## Graph databases

**Examples:** Neo4j, Amazon Neptune, Memgraph, ArangoDB (multi-model).

Data is nodes and edges; queries traverse relationships ("friends of friends who like X", "shortest
path", "what does this account connect to?").

- **Great at:** deep, variable-length relationship queries: recommendations, fraud rings, network
  topology, knowledge graphs.
- **Weaker at:** simple CRUD at scale, aggregates.
- **Tip:** for shallow hierarchies (comments, org charts), SQL recursive CTEs (`WITH RECURSIVE`) are
  often enough.

## Vector databases

**Examples:** pgvector (PostgreSQL extension), Qdrant, Weaviate, Milvus, Pinecone; also vector
support in Elasticsearch/OpenSearch and Redis.

Store **embeddings** — arrays of numbers produced by ML models that place similar items close
together — and answer "find the K nearest vectors" with approximate nearest-neighbour indexes such as
HNSW.

- **Great at:** semantic search, recommendations, retrieval for LLM applications (RAG),
  de-duplication of similar images/text.
- **Weaker at:** everything else; usually combined with metadata filters from another store.
- **Tip:** if you already run Postgres and have up to millions of vectors, pgvector keeps vectors next
  to the data they describe, inside the same transactions.

## Distributed SQL ("NewSQL")

**Examples:** CockroachDB, YugabyteDB, TiDB, Google Spanner.

SQL and transactions with data automatically sharded and replicated across nodes using consensus
(Raft/Paxos). You get horizontal scale and survivability of node or zone failures without hand-written
sharding — at the cost of higher latency per write (consensus round trips) and more complex
operations.

## A decision process

```text
Is it a cache, session store, counter or lock?          -> key-value (Redis/Valkey)
Is it analytics over large history?                     -> columnar OLAP, fed from your main DB
Is it a search box with relevance and typos?            -> Postgres FTS, then a search engine if needed
Is it metrics/measurements over time?                   -> time-series (or Timescale on Postgres)
Is it semantic similarity / embeddings?                 -> pgvector, then a vector DB if needed
Deep relationship traversals are the core feature?      -> graph DB
Huge write volume, simple key-based access, many nodes? -> wide-column (Cassandra/Scylla)
Everything else (most business data)                    -> relational: PostgreSQL or MySQL
```

### Why "start with PostgreSQL" is usually right

- One system to operate, back up and secure.
- Transactions and constraints protect you from whole classes of bugs.
- Extensions cover search, JSON, geo, time-series and vectors well enough for a long time.
- A single well-provisioned Postgres server handles far more load than most products ever reach.

Add a specialised store when you have **measured** a need the general one cannot meet: a query that
stays too slow after indexing, write volume beyond one primary, search relevance your users complain
about. Each extra database adds operational work *and* a consistency problem: data now lives in two
places and must be kept in sync, usually through a [transactional outbox or change data capture](/posts/distributed-transactions-saga-outbox).

## Common mistakes

- **Choosing by hype or benchmark.** Benchmarks measure someone else's workload.
- **Picking NoSQL "for scale" on day one** and then reimplementing joins, transactions and constraints
  in application code.
- **Using a search engine or cache as the source of truth.** Rebuildable stores should be rebuildable.
- **Ignoring operations.** A database nobody on the team understands is a risk, however fast it is.

## Further reading

- Martin Kleppmann, *Designing Data-Intensive Applications* — chapters 2 (data models) and 3 (storage)
- [DB-Engines ranking](https://db-engines.com/en/ranking) — a catalogue of database systems by category
- PostgreSQL docs: [Full Text Search](https://www.postgresql.org/docs/current/textsearch.html) and [JSON types](https://www.postgresql.org/docs/current/datatype-json.html)
- [pgvector](https://github.com/pgvector/pgvector) on GitHub
