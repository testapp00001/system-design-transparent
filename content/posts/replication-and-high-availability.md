+++
title = "Database replication and high availability: replicas, failover and split brain"
summary = "How primary-replica replication works, synchronous vs asynchronous trade-offs, replication lag and read-your-writes, automatic failover with Patroni or managed services, and the split-brain problem."
tags = ["database", "postgresql", "mysql", "reliability", "distributed-systems"]
level = "advanced"
date = 2026-10-02
+++

A single database server is a single point of failure. When its disk dies, the whole product is down
— and if there's no copy, the data is gone. Replication keeps copies of the data on other servers;
high availability (HA) uses those copies to keep serving when a server fails. They sound simple. The
details are where systems lose data.

## Primary-replica (leader-follower) replication

The standard setup for PostgreSQL and MySQL:

```text
             writes + reads
 app  ------------------------->  [ primary ]
  |                                   |  change stream (WAL / binlog)
  |  reads (optional)                 v
  +------------------------------> [ replica 1 ]   [ replica 2 ]
```

- All **writes** go to the **primary** (leader).
- The primary streams its changes to **replicas** (followers), which apply them in the same order.
  PostgreSQL streams its **WAL** (write-ahead log) — physical, byte-level changes. MySQL streams its
  **binary log** (binlog) — logical row changes (or statements).
- Replicas can serve **reads** and can be **promoted** to primary if the primary fails.

PostgreSQL also offers **logical replication** (publishing changes per table, usable across major
versions and for selective replication), and both databases are used as sources for change data
capture.

## Synchronous vs asynchronous

The central trade-off: when does the primary tell the client "committed"?

- **Asynchronous** (the default in both): as soon as the change is durable on the primary. Replicas
  catch up shortly after — usually milliseconds, sometimes seconds or more under load.
  - Fast writes; replica problems don't slow the primary.
  - **If the primary dies, the last few transactions that weren't yet replicated are lost** when a
    replica is promoted.
- **Synchronous**: only after at least one replica has also received (or applied) the change.
  - No data loss on failover (for the synchronous replica).
  - Every commit waits for a network round trip; if the synchronous replica is down or slow, writes
    stall — so you typically run **two or more** candidates and require any one of them
    (PostgreSQL: `synchronous_standby_names = 'ANY 1 (r1, r2)'`).
- **Semi-synchronous** (MySQL) and quorum settings sit in between.

This trade-off is described by two numbers every system should have:

- **RPO (recovery point objective)**: how much data you can afford to lose. Async replication → RPO
  of seconds; sync → RPO ≈ 0.
- **RTO (recovery time objective)**: how long you can afford to be down. Automatic failover → RTO of
  tens of seconds; manual → however long it takes to wake someone up.

## Replication lag and read-your-writes

Sending reads to replicas scales read traffic, but replicas are slightly behind. Classic bug:

```text
1. User updates their profile          -> primary
2. Page reloads, reads the profile     -> replica (hasn't applied the update yet)
3. User sees the old data and thinks the save failed
```

Solutions:

- **Read-your-writes**: after a user writes, route *their* reads to the primary for a short time
  (e.g. a few seconds, tracked in their session), or until the replica has caught up to the position
  of their write (PostgreSQL LSN, MySQL GTID).
- Send reads that need fresh data (checkout, balances, anything followed by a write) to the primary;
  send reads that tolerate staleness (listings, search, reports) to replicas.
- **Monitor lag** and take a replica out of rotation if it falls too far behind.

Also: **monotonic reads** — a user bouncing between replicas with different lag can see data "go back in
time". Pin a session to one replica to avoid it.

## Failover

When the primary fails, a replica must be promoted, and clients must start writing to it:

1. **Detect** that the primary is really down (not just slow, or a network blip between the monitor
   and the primary).
2. **Choose** the most up-to-date replica.
3. **Promote** it to primary.
4. **Fence** the old primary so it can't accept writes if it comes back.
5. **Redirect** clients — via a virtual IP, DNS, a proxy (HAProxy, PgBouncer, ProxySQL), or a
   cluster-aware driver.
6. **Reattach** other replicas to the new primary.

Doing this by hand at 3 a.m. is slow and error-prone. Tools automate it:

- **PostgreSQL**: Patroni (uses etcd/Consul/ZooKeeper to elect a leader), pg_auto_failover, repmgr;
  CloudNativePG and others on Kubernetes.
- **MySQL**: Group Replication / InnoDB Cluster, Orchestrator, MHA.
- **Managed databases** (Amazon RDS/Aurora, Google Cloud SQL, Azure Database, and others) provide
  failover as a feature — one of the strongest reasons to use them.

## Split brain: the failure that corrupts data

The nightmare scenario: a network partition separates the primary from the rest. The failover system
can't see the primary, promotes a replica — but the old primary is still alive and some clients still
write to it. Now there are **two primaries** accepting different writes. Merging them afterwards is
painful or impossible.

Defences:

- **Consensus-based leader election** (Patroni with etcd, Raft-based systems): a node may only act as
  primary while holding a lease from a majority quorum. A primary that can't renew its lease steps
  down (demotes itself).
- **Fencing**: make sure the old primary *cannot* write — revoke its access, shut it down (STONITH:
  "shoot the other node in the head"), or have storage reject it.
- **An odd number of voters across failure domains**: with 3 or 5 nodes in different zones, only one
  side of a partition can have a majority.

## Beyond a single primary

- **Multi-primary (multi-leader)** replication lets several nodes accept writes (e.g. one per region)
  but requires **conflict resolution** when two writes to the same data collide — last-write-wins
  (silently losing data), custom merge logic, or CRDTs. Use it only when you really need writes in
  multiple regions with low latency.
- **Leaderless / quorum** systems (Cassandra, DynamoDB-style) write to and read from several replicas
  (`W + R > N` for overlapping quorums) — see [CAP and consistency models](/posts/cap-theorem-and-consistency-models).
- **Distributed SQL** (CockroachDB, YugabyteDB, Spanner) replicate each range of data with Raft/Paxos,
  making failover automatic and safe at the cost of write latency.

## Replication is not a backup

Replication copies **everything**, including mistakes: `DROP TABLE` or a buggy migration on the
primary is faithfully replayed on every replica within milliseconds. You still need
[backups with point-in-time recovery](/posts/database-backups-and-recovery). (A *delayed* replica, kept
an hour behind on purpose, can help recover from such mistakes quickly — but it's a supplement, not a
replacement.)

## Checklist

- [ ] At least one replica in a different failure domain (zone).
- [ ] Defined RPO and RTO, and a replication mode that matches them.
- [ ] Automatic failover with quorum-based leader election and fencing — or a managed service.
- [ ] Clients reconnect to the new primary automatically (test it!).
- [ ] Replication lag is monitored and alerted on.
- [ ] Reads that must be fresh go to the primary.
- [ ] Failover is rehearsed regularly, not first tried during an outage.

## Further reading

- PostgreSQL docs: [High Availability, Load Balancing, and Replication](https://www.postgresql.org/docs/current/high-availability.html)
- MySQL docs: [Replication](https://dev.mysql.com/doc/refman/8.0/en/replication.html)
- [Patroni documentation](https://patroni.readthedocs.io/)
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapter 5 (Replication)
- GitHub Engineering (2018): [October 21 post-incident analysis](https://github.blog/2018-10-30-oct21-post-incident-analysis/) — a real cross-region failover incident
