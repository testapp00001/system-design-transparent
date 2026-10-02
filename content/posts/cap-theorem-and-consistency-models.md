+++
title = "CAP, PACELC and consistency models without the confusion"
summary = "What the CAP theorem actually says (and doesn't), why PACELC is more useful day to day, the spectrum from linearizable to eventual consistency, and how quorums let you tune it."
tags = ["distributed-systems", "database", "system-design"]
level = "advanced"
date = 2026-10-02
+++

"Pick two of consistency, availability and partition tolerance" is one of the most quoted — and most
misunderstood — ideas in system design. This article explains what CAP really says, why a related
idea (PACELC) matters more in daily decisions, and what the different "consistency" levels mean for
your users.

## The setting: data copied across machines

As soon as data lives on more than one machine (replicas for availability, or nodes for scale), a
question appears: when a client writes to one copy, what do readers of the *other* copies see, and
when?

## CAP, precisely

The CAP theorem (Eric Brewer's conjecture, proved by Gilbert and Lynch in 2002) considers three
properties:

- **C — Consistency**, in the specific sense of **linearizability**: the system behaves as if there
  were a single copy of the data; once a write completes, every later read sees it.
- **A — Availability**: every request to a non-failed node gets a (non-error) response.
- **P — Partition tolerance**: the system keeps working even if the network between nodes drops
  messages.

The theorem: **when a network partition happens, a system must choose between consistency and
availability.** Either a node that can't reach the others refuses requests (stays consistent, loses
availability), or it answers with possibly stale data (stays available, loses consistency).

What CAP does **not** say:

- It doesn't mean "pick any two" as a design menu. Partitions are not optional in a real network —
  cables, switches, and cloud networks fail. So the real choice is **C or A, during a partition**.
- It says nothing about the normal case, when the network is fine — which is most of the time.
- Its "consistency" is one strong definition; its "availability" is all-or-nothing. Real systems sit
  in between and are tuned per operation.

```text
       network partition separates node A from nodes B and C
   client --> [A]   x x x x   [B] [C] <-- client
   A receives a write. Should A accept it (A: available, but B/C won't see it -> inconsistent)
   or reject it (consistent, but unavailable for this client)?
```

Examples of the choice:

- **CP** behaviour: etcd, ZooKeeper, Consul (Raft/Zab-based), Spanner, CockroachDB — the minority side
  of a partition stops accepting writes. A single-primary PostgreSQL setup is CP-like: a client that
  can't reach the primary can't write.
- **AP** behaviour: Cassandra and DynamoDB-style stores at low consistency settings, DNS, many caches —
  they keep answering and reconcile later.

## PACELC: the more useful question

Daniel Abadi's PACELC extends CAP: **if Partition, choose Availability or Consistency; Else (normal
operation), choose Latency or Consistency.**

The "else" part is the everyday trade-off. Keeping replicas strongly consistent requires coordination
— waiting for other nodes, possibly in other data centers — on every write (and sometimes every read).
That costs **latency**, all the time, not only during rare partitions.

- Synchronous replication, consensus writes → consistent, slower.
- Asynchronous replication, reads from local replicas → fast, possibly stale.

Most design discussions are really PACELC "else" questions: *is this read allowed to be a little stale
in exchange for being fast and local?*

## The consistency spectrum

From strongest (easiest to reason about, most expensive) to weakest:

| Model | What clients see | Example of where it's used |
|---|---|---|
| **Linearizable** | One copy of the data, real-time order | Leader election, locks, unique usernames, bank balances |
| **Sequential** | All see the same order of operations, not necessarily real-time | Some coordination services |
| **Causal** | Effects are never seen before their causes (a reply never before the message it answers) | Comment threads, collaborative apps |
| **Read-your-writes** | You always see your own writes | Profile updates, settings |
| **Monotonic reads** | You never see data go "back in time" | Any UI reading from replicas |
| **Eventual** | If writes stop, replicas converge eventually; meanwhile anything goes | Like counts, view counts, caches, DNS |

The weaker guarantees in the middle — read-your-writes, monotonic reads, causal — are **session
guarantees** that you can often provide cheaply on top of an eventually consistent system, and they
remove most of the visible weirdness for users (see [replication](/posts/replication-and-high-availability)
for read-your-writes with replicas).

> [!NOTE]
> "Consistency" in CAP is not the "C" in ACID. ACID's C means "constraints hold"; CAP's C means
> "linearizable". Isolation levels (serializable, snapshot…) are yet another axis — see
> [transactions and isolation](/posts/transactions-and-isolation-levels).

## Quorums: tuning consistency per request

Leaderless systems such as Cassandra (and the original Amazon Dynamo design) replicate each item to
**N** nodes. A write waits for **W** acknowledgements, a read queries **R** nodes and takes the newest
value.

```text
N = 3 replicas
W + R > N  -> read and write sets overlap -> a read sees the latest acknowledged write
               e.g. W=2, R=2 ("QUORUM")
W = 1, R = 1 -> fastest, but reads may miss recent writes (eventual)
W = 3        -> every write needs all replicas: slow, and fails if any replica is down
```

Cassandra exposes this per query (`ONE`, `QUORUM`, `ALL`, `LOCAL_QUORUM`…), so the same database can
serve a fast, eventually consistent read for a timeline and a quorum read for something more
sensitive. (Even with overlapping quorums there are edge cases — concurrent writes, clock-based
last-write-wins conflict resolution — so "quorum" is not automatically linearizable.)

## How to decide, per piece of data

Ask: **what goes wrong for the user or the business if a read is stale or two writes conflict?**

- Money, inventory you sell against, unique constraints (usernames, seat booking), permissions
  revocation, leader election → **strong consistency**. Route to the primary/leader, use transactions or
  consensus.
- A user's own recent changes → **read-your-writes**.
- Counters, likes, recommendations, analytics, search indexes, caches → **eventual** is fine, often
  great, and much cheaper.

Most systems mix all of these. The skill is being explicit about which data needs which guarantee,
rather than making everything strongly consistent (slow, expensive) or everything eventual (subtle
bugs).

## Key takeaways

- CAP: during a network partition, choose consistency or availability. Partitions will happen.
- PACELC: in normal operation, choose latency or consistency — the everyday trade-off.
- Consistency is a spectrum; session guarantees (read-your-writes, monotonic reads) fix most user-visible
  anomalies cheaply.
- Decide per data type, based on the cost of staleness and conflicts.

## Further reading

- Martin Kleppmann: [Please stop calling databases CP or AP](https://martin.kleppmann.com/2015/05/11/please-stop-calling-databases-cp-or-ap.html)
- Daniel Abadi: *Consistency Tradeoffs in Modern Distributed Database System Design* (IEEE Computer, 2012) — the PACELC paper
- Eric Brewer: [CAP Twelve Years Later: How the "Rules" Have Changed](https://www.infoq.com/articles/cap-twelve-years-later-how-the-rules-have-changed/)
- [Jepsen: Consistency Models](https://jepsen.io/consistency)
- DeCandia et al., *Dynamo: Amazon's Highly Available Key-value Store* (SOSP 2007)
