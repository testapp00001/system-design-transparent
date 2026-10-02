+++
title = "Consensus and leader election: how Raft keeps replicas in agreement"
summary = "Why replicated systems need consensus, how majority quorums decide cluster size, how Raft elects a leader and replicates a log safely, how leases prevent split brain, and where you already depend on it (etcd, Consul, ZooKeeper, Kafka)."
tags = ["distributed-systems","reliability"]
level = "advanced"
date = 2026-10-02
+++

You run Kubernetes with three control-plane nodes. One dies during the night and nobody notices,
because everything keeps working. A month later a second node dies, and suddenly every `kubectl apply`
fails, even though the third node, with a full copy of the data, is healthy. That node refuses to
accept changes alone, because it cannot tell whether the other two are dead or just cut off and
still working. That refusal is **consensus**
doing its job. This article explains how it works, using **Raft**, the algorithm inside etcd (where
Kubernetes stores its cluster state) and many other systems.

## The problem: agreeing on an ordered log

Systems keep copies of their data, called **replicas**, on several machines (see
[replication and high availability](/posts/replication-and-high-availability)). The cleanest way to
keep them identical is a **replicated state machine**: every change is a command ("set x = 5") in a
**log**, an append-only list with numbered positions. If every replica applies the same commands in
the same order, every replica ends up in the same state.

The hard part: **all replicas must agree on which command sits at each position of the log**, even
when some crash, run slowly, or cannot reach each other. That is the **consensus** problem. You cannot
tell a slow node from a dead one, messages get lost, and **two leaders are worse than none**: if two
nodes both accept writes, the copies split apart. This **split brain** silently corrupts data.

Raft, Paxos and Zab assume **crash failures**: nodes may crash or pause and messages may be lost, but
no node lies (handling lying, "Byzantine", nodes needs other algorithms). They promise:

- **Safety**: a committed entry is never lost or changed, whatever the timing.
- **Liveness**: progress, while a majority of nodes are up and can talk to each other.

## Majority quorums: why clusters have 3 or 5 nodes

A **quorum** is the number of nodes that must agree before a decision counts. Raft uses a
**majority**: more than half of the voting members. The key property: **any two majorities of the
same cluster share at least one node.** So whatever one majority decided, every later majority
includes a node that knows about it. All of Raft's safety rests on this overlap.

To survive `f` failed nodes, you need `2f + 1` nodes:

| Cluster size | Majority | Failures tolerated |
|---|---|---|
| 2 | 2 | 0 |
| 3 | 2 | **1** |
| 4 | 3 | 1 |
| 5 | 3 | **2** |

- **Even sizes do not help.** 4 nodes tolerate one failure, like 3, but each write waits for more
  nodes. A 2-node cluster is *less* available than one node: losing either node stops it.
- **More nodes is not faster.** Every write still waits for a majority. Most production clusters
  use **3** or **5** voting members.

## Raft: roles and terms

Diego Ongaro and John Ousterhout published Raft in 2014 with an explicit goal: to be easier to
understand than Paxos while giving the same guarantees. Each node is in one of three roles:

- **Follower**: passive; accepts entries from the leader and answers vote requests.
- **Candidate**: stopped hearing from a leader and is asking for votes.
- **Leader**: handles all client writes and replicates them.

```text
                   election timeout
                   (no word from a leader)
          +----------+ ----------------------> +-----------+ --+
start --> | FOLLOWER |                         | CANDIDATE |   | election timeout
          +----------+ <---------------------- +-----------+ <-+ (split vote): new term
             ^         discovers the leader          |
             |         or a higher term              | votes from a majority
             |                                       v
             |         discovers a higher term   +--------+
             +---------------------------------- | LEADER |
                                                 +--------+
```

Time is divided into numbered **terms**, each starting with an election, with at most one leader per
term. Terms act as a logical clock (see [clocks and ordering](/posts/ids-clocks-and-ordering)): every
message carries the sender's term, a node that sees a **higher** term adopts it and becomes a
follower (even a leader), and messages with an **older** term are rejected.

## Leader election

The leader regularly sends **heartbeats** (empty AppendEntries messages). A follower that hears
nothing for its **election timeout** increases its term, becomes a candidate, votes for itself and
sends `RequestVote` to the others. Each node gives **at most one vote per term**, saved to disk so a
restart cannot make it vote twice. A candidate with votes from a majority becomes leader. If votes
are split, candidates time out and try again in a new term.

**Randomised timeouts** make split votes rare: each node picks its timeout at random from a range (the
Raft paper gives 150–300 ms as an example), so usually one node times out first and wins alone.

The most important rule: **a node refuses to vote for a candidate whose log is less up to date than
its own** (its last entry has a lower term, or the same term and a lower index). Simplified:

```python
def on_request_vote(node, req):
    if req.term > node.current_term:          # newer term: forget the old vote
        node.current_term, node.voted_for, node.role = req.term, None, "follower"
    if req.term < node.current_term:          # candidate from an old term
        return Vote(node.current_term, granted=False)
    log_ok = (req.last_log_term, req.last_log_index) >= \
             (node.last_log_term(), node.last_log_index())   # term first, then index
    granted = node.voted_for in (None, req.candidate_id) and log_ok
    if granted:
        node.voted_for = req.candidate_id
        node.reset_election_timer()
    node.persist()                            # term and vote must survive a crash
    return Vote(node.current_term, granted=granted)
```

## Log replication and the commit index

A write goes like this (followers redirect clients to the leader):

1. The leader appends the command to its own log and sends it to all followers in `AppendEntries`.
2. Each follower writes it to disk and replies.
3. Once the entry is on a **majority** (counting the leader), it is **committed**. The leader advances
   its **commit index** (the highest position known to be on a majority), applies the command to its
   state machine, and answers the client.
4. The next `AppendEntries` tells followers the new commit index, and they apply the entry too.

A 5-node cluster in term 3 might look like this (each box is one entry, labelled with its term):

```text
index:          1    2    3    4    5    6
              +----+----+----+----+----+----+
leader   S1   | t1 | t1 | t2 | t3 | t3 | t3 |   commit index = 5
              +----+----+----+----+----+----+
follower S2   | t1 | t1 | t2 | t3 | t3 | t3 |
follower S3   | t1 | t1 | t2 | t3 | t3 |        index 5 is on S1, S2, S3: committed
follower S4   | t1 | t1 | t2 |                  slow: the leader will resend 4-6
follower S5   | t1 | t1 |                       down
```

Index 6 is on only 2 of 5 nodes, so it is not committed. If S1 crashes now, entry 6 may be lost,
which is fine: the client never got "OK" for it.

Each `AppendEntries` also carries the index and term of the previous entry. A follower accepts only if
its log matches there; otherwise the leader steps back until both logs agree and overwrites the
follower's log after that point. Leaders never delete their own entries. Nodes also take
**snapshots** so the log does not grow forever. Change membership one member at a time, with your
system's own commands.

## Why it is safe: the intuition

Follow one committed entry through a leader change:

1. The entry is committed, so it is stored on a **majority**.
2. A new leader needs votes from a **majority**.
3. The two majorities **overlap** in at least one node, which has the entry.
4. That node refuses to vote for a candidate whose log is less up to date than its own.
5. So every new leader already has every committed entry (**Leader Completeness**).

A subtle extra rule: a leader only commits entries **from its own term** by counting replicas; older
entries become committed indirectly when a later one is. That is one reason a new leader starts its
term with an empty "no-op" entry.

Notice that this argument never uses clocks. Timing affects only liveness: in bad network
conditions Raft may stop making progress, but it never gives up safety.

## Old leaders, leases and split brain

During a network partition, an old leader may not know yet that it was replaced:

```text
                   partition
   [S1 leader, term 3]  [S2]   |   [S3]  [S4]  [S5]
   2 of 5: a minority          |   3 of 5: elect S3 as leader in term 4
   its writes cannot commit    |   writes commit normally
```

**Writes stay safe.** S1 cannot reach a majority, so nothing it accepts commits; its clients time out
and retry (make writes [idempotent](/posts/retries-timeouts-and-idempotency)). When the partition
heals, S1 sees term 4, steps down, and its uncommitted entries are overwritten.

**Reads are the danger.** If S1 answers reads from its own copy, clients see stale data:

| Technique | How it works | Cost |
|---|---|---|
| ReadIndex | Leader records its commit index, confirms with a heartbeat round that a majority still follows it, waits until it has applied entries up to that index, then answers | One round trip (shared by many reads) |
| Lease read | After a successful heartbeat round, the leader assumes no new leader can exist for about one election timeout, and answers locally | Fast, but relies on clocks running at similar speeds and no long process pauses |
| Follower / "serializable" read | Any node answers from its local copy | Fastest, may be stale |

etcd reads are linearizable by default (it uses ReadIndex); a "serializable" read is answered from
the local copy of whichever member you ask. Two common extensions also help: with **CheckQuorum**, a
leader that has not heard from a majority for an election timeout steps down; with **PreVote**, a
node returning from a partition asks "would you vote for me?" before raising its term, so it does not
force a needless election.

A **leader lease** ("I am the leader until time T") is also how *your* application can elect a
leader: hold a lease in etcd, an ephemeral node in ZooKeeper (it disappears when your session ends)
or a Kubernetes `Lease` object, and do leader-only work while you hold it. But a paused process
(garbage collection, a frozen VM) may act after its lease expired. So also send a **fencing token** (a number that grows with every new leader, like the Raft term) to
the system you write to, and make it reject older tokens (see [distributed locks](/posts/distributed-locks)).

## Where you meet consensus

| System | Protocol | Used for |
|---|---|---|
| **etcd** | Raft | All Kubernetes cluster state; one of the stores Patroni (PostgreSQL failover) can use |
| **Consul** | Raft (among server agents) | Service discovery, configuration, key-value data |
| **ZooKeeper** | Zab (ZooKeeper Atomic Broadcast) | Coordination for HBase, Solr, Hadoop, older Kafka |
| **Kafka (KRaft mode)** | A Raft-based protocol | Cluster metadata, replacing ZooKeeper |
| **CockroachDB, TiKV** | Raft, one group per range of data | Distributed SQL and key-value storage |

- **Kafka** replaced ZooKeeper through KIP-500: a small group of controllers keeps cluster metadata in
  a Raft-based log, and Kafka 4.0 removed ZooKeeper mode. Topic data is still replicated with Kafka's
  own in-sync-replica mechanism (see [message queues and event streams](/posts/message-queues-and-event-streams)).
- **CockroachDB and TiKV** run a separate Raft group for each range of data ("multi-Raft"), so writes
  spread over many leaders (see [sharding](/posts/sharding-and-partitioning)).

**Paxos**, by Leslie Lamport, is the older, more famous algorithm (Google's Chubby and Spanner use
variants of it). The Raft paper describes Raft as equivalent to (multi-)Paxos, but easier to
understand. Zab uses similar ideas too: a leader, epochs (like terms) and majorities.

## Trade-offs and when not to use it

- **Latency.** Every write waits for a round trip to a majority plus a disk flush (`fsync`). Across
  regions that means tens or hundreds of milliseconds per write.
- **Throughput.** One leader handles all writes for a group.
- **Availability.** The minority side of a partition stops accepting writes, the deliberate "C over A"
  choice from [CAP](/posts/cap-theorem-and-consistency-models).
- **Small data.** Every etcd, ZooKeeper or Consul member stores all the data (etcd's default storage
  limit is 2 GB). They hold metadata and leases, not your orders table.

You may not need a consensus cluster at all: managed databases have failover built in, "only one
instance runs this job" can be a database row lock (see [background jobs](/posts/background-jobs-and-cron);
this is safest when the job also writes to that same database), and counters or caches can tolerate
conflicts.

## Never implement it yourself

The paper fits Raft's core rules in one figure. Production also needs crash-safe storage, snapshots,
membership changes, PreVote, efficient reads and flow control. Google's *Paxos Made Live* paper
describes how much engineering Paxos took in practice, and Jepsen's independent tests have found
consistency bugs in several well-known consensus-based systems.

Use etcd, ZooKeeper, Consul, or a database with consensus inside. If you build infrastructure and
need a library, choose a heavily used one, such as etcd's or HashiCorp's raft library. Never build
"leader election" from a heartbeat timestamp in Redis or a database row: that is an unfenced lease.

## In practice

- **3 members** for most needs, **5** to survive two failures (or one during maintenance).
- **One member per failure domain**: three availability zones for three members.
- **Fast, dedicated disks.** Slow `fsync` (or a slow network) causes missed heartbeats and repeated
  elections.
- **Tune timeouts to the network.** etcd defaults to a 100 ms heartbeat interval and a 1,000 ms
  election timeout; its tuning guide explains when to raise them.
- **Watch leader changes.** Frequent elections point to slow disks, network trouble or overload.
- **Back it up.** An etcd snapshot backs up all your Kubernetes objects (but not the data inside
  your persistent volumes).
- **Use non-voting members, not more voters.** ZooKeeper *observers* receive every change but do not
  vote, so you can add them to serve more reads without slowing writes. etcd *learners* are also
  non-voting; their main use is adding a new member safely: it copies the data first and gets a
  vote only after you promote it.

```sh
etcdctl member list -w table                 # members and their IDs
etcdctl endpoint status --cluster -w table   # leader, raft term and raft index per member
etcdctl snapshot save /backup/etcd.db        # snapshot for backups (ask one member only)
```

On a real cluster you also pass `--endpoints` and, usually, the TLS flags (`--cacert`, `--cert`,
`--key`).

## Common mistakes

- **Two-node "HA" clusters**, or all members in one zone.
- **Stretching a cluster across distant regions** without raising timeouts.
- **Stale reads by accident**: serializable or follower reads for data that must be current.
- **Trusting a lease without fencing tokens.**
- **Panicked recovery.** etcd's `--force-new-cluster` turns one member into a new one-member cluster
  with only the data that member has. If that member was behind, writes that were already committed
  on the other members are lost. Practise restoring from a snapshot before you need it.

## Further reading

- [The Raft Consensus Algorithm](https://raft.github.io/): papers, a visualisation, implementations
- Diego Ongaro and John Ousterhout: [In Search of an Understandable Consensus Algorithm](https://raft.github.io/raft.pdf)
  (extended version; a shorter version appeared at USENIX ATC 2014), and Ongaro's PhD thesis
  *Consensus: Bridging Theory and Practice*
- [etcd documentation](https://etcd.io/docs/), especially the tuning and disaster recovery pages
- *KIP-500: Replace ZooKeeper with a Self-Managed Metadata Quorum* (Apache Kafka wiki)
- Leslie Lamport: *Paxos Made Simple*; Chandra, Griesemer and Redstone: *Paxos Made Live: An
  Engineering Perspective*
- Martin Kleppmann, *Designing Data-Intensive Applications* (first edition), chapter 9
  (Consistency and Consensus)
