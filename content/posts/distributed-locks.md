+++
title = "Distributed locks and their pitfalls (and when you don't need one)"
summary = "Why teams add locks across servers, how database, Redis and etcd/ZooKeeper locks work, why pauses, clocks and partitions break them, and how fencing tokens and lock-free designs keep data correct."
tags = ["distributed-systems","reliability"]
level = "advanced"
date = 2026-10-02
+++

Your service runs on three instances. Every night each of them wakes up and sends invoices, so every
customer gets three. Someone says: "Let's take a lock in Redis first." It works in testing. Months
later, on a night with a long garbage-collection pause, two instances still run the "protected" code
at the same time, and nobody understands how, because "we had a lock".

This article explains what a distributed lock really gives you, how it fails, and how to design so
that you often don't need one.

## Why teams reach for a lock

Inside one process, a **mutex** (mutual-exclusion lock) makes sure only one thread runs a piece of
code at a time (see [concurrency models](/posts/concurrency-models)). With many servers, an
in-memory mutex doesn't help: each server has its own. A **distributed lock** lives somewhere all
servers can reach (a database, Redis, ZooKeeper, etcd), so only one server at a time does the work.

Typical reasons: a [scheduled job](/posts/background-jobs-and-cron) that every instance would
otherwise run, an expensive [cache rebuild](/posts/caching-strategies), a read-modify-write on a file
in object storage, or one "leader" instance that consumes a feed.

A distributed lock almost always has a time limit. If the holder crashes, the lock must not stay
taken forever, so it expires. A lock with an expiry time is called a **lease**. That expiry is
necessary, and it is also the root of most problems below.

## Efficiency locks vs correctness locks

Martin Kleppmann, in his 2016 article on distributed locking, suggests first asking *why* you need
the lock:

| | Efficiency lock | Correctness lock |
|---|---|---|
| Purpose | Avoid doing the same work twice | Prevent wrong or corrupt data |
| Example | One server rebuilds a cache entry | One server debits an account |
| If two holders run at once | Wasted CPU, a duplicate email | Double charge, lost update |

So ask: **"What happens if, very rarely, two holders run at the same time?"** If the answer is "a bit
of waste", any reasonable lock is fine. If the answer is "we lose money", a lock alone is not enough.

## Option 1: database row locks

If you protect a row in your database, the database already has the right tool.
`SELECT ... FOR UPDATE` locks the selected rows until the transaction ends:

```sql
BEGIN;
SELECT balance FROM accounts WHERE id = 42 FOR UPDATE;   -- other transactions wait here
-- the application checks the balance
UPDATE accounts SET balance = balance - 80 WHERE id = 42;
INSERT INTO ledger (account_id, amount) VALUES (42, -80);
COMMIT;                                                  -- lock released
```

This lock has a property the others lack: **the lock and the data share the same fate.** If your
server freezes or its connection breaks, the transaction is either still open (still holding the
lock, so nobody else gets in) or rolled back (so its writes never happen). Two holders can never both
commit. The worst case is waiting, not corruption.

The limits: it only protects data in that database, and holding it during slow work (an external
API call) makes other transactions pile up. See [transactions and isolation levels](/posts/transactions-and-isolation-levels).

## Option 2: PostgreSQL advisory locks

When the thing to lock is not a row ("the nightly invoice run"), PostgreSQL **advisory locks**
help. Each one is identified by a number you choose (a 64-bit integer); your code decides what it
protects.

```sql
-- transaction-level: released automatically at COMMIT or ROLLBACK
BEGIN;
SELECT pg_try_advisory_xact_lock(1001);   -- true: we hold it; false: someone else does, so skip the work
-- ... do the work in this same transaction ...
COMMIT;

-- session-level: held until unlocked, or until the connection closes
SELECT pg_try_advisory_lock(1001);
-- ... work ...
SELECT pg_advisory_unlock(1001);
```

The `try` versions return `false` immediately instead of waiting. Define the key numbers in one
place so two features never share one by accident.

The transaction-level version keeps the shared-fate property, *if* the work is writes in the same
transaction. Session-level locks are riskier. Behind a pooler such as PgBouncer in transaction mode,
the lock and the unlock may run on different server connections (see
[connection pooling](/posts/connection-pooling)). And if the session dies while your code works
*outside* the database, PostgreSQL releases the lock, and another instance can start while the first
one is still busy.

## Option 3: Redis `SET NX PX` with a token

A very common lock is a single Redis key:

```text
SET lock:invoice-run 3f9c2b7e-5d1a-4c8e-9b0f-6a2d7e1c4b85 NX PX 30000
```

- `NX`: only set the key if it does **not** exist. If it exists, someone else holds the lock.
- `PX 30000`: the key expires after 30,000 ms, so a crashed holder can't block everyone forever.
- The value is a **random token**, unique to this attempt (for example a UUID).

The token matters when you release. Suppose your work took longer than 30 seconds: your key expired,
another instance took the lock, and now you run `DEL lock:invoice-run`. You just deleted *their*
lock. Instead, release with a check-and-delete that runs atomically on the server, as a Lua script:

```lua
-- KEYS[1] = the lock key, ARGV[1] = my token
if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("DEL", KEYS[1])
else
    return 0   -- not my lock any more: do nothing
end
```

A `GET` followed by a `DEL` from your application is not enough: the key can change hands between
the two commands. A similar script with `PEXPIRE` instead of `DEL` extends a lease you still hold.
Since Redis 8.4, the `DELEX` command can do the same check-and-delete in one command
(`DELEX lock:invoice-run IFEQ <my token>`); on older versions, use the script.

Two limits remain. **Failover:** Redis replication is asynchronous, so if the primary grants your lock
and crashes before the key reaches the replica, the promoted replica has no lock key and a second
client can take it. The Redis documentation points this out itself. **Expiry:** your code cannot see,
in the middle of its work, that the key has expired.

The Redis documentation also describes **Redlock**, which takes the lock on a majority of several
independent Redis primaries (its example uses five), and recommends it over the single-key pattern.
Redlock is the subject of a famous debate, below.

## Option 4: leases on ZooKeeper or etcd

ZooKeeper and etcd are small, strongly consistent stores. They replicate every write to a majority of
nodes with a consensus protocol (ZAB in ZooKeeper, Raft in etcd; see
[consensus and leader election](/posts/consensus-and-leader-election)), so an acknowledged lock is
not lost when one node fails.

- **ZooKeeper**: the lock recipe creates an *ephemeral sequential* node such as
  `/locks/invoice/lock-0000000042`. The lowest number holds the lock; the others each watch the node
  just before their own. When the holder's session expires (it stopped sending heartbeats), its
  ephemeral node is deleted and the next client takes over. Apache Curator (Java) packages this as
  `InterProcessMutex`.
- **etcd**: a client creates a **lease** with a TTL (time to live) and sends keep-alive messages.
  Keys attached to the lease are deleted when it expires. etcd ships a lock built on this
  (`etcdctl lock` on the command line).

Both also give you numbers that **only grow**: ZooKeeper's transaction id (zxid) and etcd's
revision number. They are useful as fencing tokens (below). But consensus does *not* fix one thing:
a lease still expires, and the holder may not know.

## What goes wrong

The lock service decides that a lease has expired using *its* view of time. The holder believes it
still has the lock using *its own* view. Nothing forces the two to agree:

```text
 t=0s    A acquires the lock (30 s lease)
 t=1s    A starts a long pause                 A is frozen and cannot notice
 t=31s   the lease expires on the lock service
 t=32s   B acquires the lock, writes x = "B"   B: "I hold the lock"
 t=41s   A wakes up, writes x = "A"            A: "I still hold the lock"
         -> B's update is silently lost
```

**Process pauses.** A process can stop at any moment without noticing: a stop-the-world garbage
collection, a virtual machine paused by its hypervisor, a throttled container, memory swapped to
disk. Checking "do I still hold the lock?" just before writing doesn't help, because the pause can
happen between the check and the write.

**Clocks.** Leases depend on time. Clocks drift, and the wall clock can **jump** when NTP corrects it
or an operator changes it. If the lock server measures leases with its wall clock and that clock
jumps forward, leases expire early. The Redis documentation warns about exactly this: Redis does not
use a monotonic clock for key expiry, so a wall-clock shift can let two processes get the lock. In
your own code, measure durations with a **monotonic clock** (one that only moves forward, such as
`time.monotonic()` in Python); that fixes your side, not the server's. See
[time and ordering](/posts/ids-clocks-and-ordering).

**Network partitions and delays.** In a **network partition**, some machines can't reach others but
keep running. The holder may be cut off from the lock service yet still reach the storage. It can't
renew, the lease expires, someone else takes the lock, and the first client keeps writing. Even
without a partition, a write sent in time can be delayed and arrive after the lease has expired.

## Kleppmann's critique of Redlock

In February 2016, Martin Kleppmann published "How to do distributed locking", an analysis of Redlock.
His main points, simplified:

1. Redlock depends on **timing assumptions**: network delays, process pauses and clock errors must be
   small compared with the lease time. He described scenarios (a clock jump on one Redis node, a GC
   pause while lock replies are on the way) where two clients both believe they hold the lock.
2. For correctness, any expiring lock needs **fencing tokens** (next section), and Redlock cannot
   produce them: its random values do not increase.
3. For efficiency, a single Redis node is simpler and good enough. For correctness, use a consensus
   system such as ZooKeeper (or at least a database with good transaction guarantees), with fencing
   tokens.

Salvatore Sanfilippo (antirez), the creator of Redis, replied in "Is Redlock safe?" and disagreed
with several points. For example, he argued that a long pause *after* a client gets a lock affects
every lock with automatic expiry, not only Redlock. He also argued that a resource able to check
tokens could use Redlock's unique random value with a check-and-set, instead of an increasing
number. He did agree that Redis and Redlock implementations should use a monotonic clock.

Read both posts. The practical lesson does not depend on who won: **an expiring lock cannot
guarantee mutual exclusion on its own.** Today the Redis documentation itself advises fencing tokens
for any distributed lock.

## Fencing tokens: make the resource say no

A **fencing token** is a number that the lock service hands out with every grant, bigger every time.
The client sends it with every write. The protected resource remembers the highest token it has seen
and **rejects writes with an older one**. Kleppmann's article illustrates it with tokens 33 and 34:

```text
 t=0s    A acquires the lock, gets token 33
 t=1s    A pauses (GC)
 t=31s   the lease expires
 t=32s   B acquires the lock, gets token 34
 t=33s   B writes x with token 34          storage: 34 is the highest so far -> accept
 t=41s   A writes x with token 33          storage: 33 < 34 -> REJECT
```

A's pause no longer matters. In SQL, the check is a conditional update (the `fence_token` column
starts at 0):

```sql
UPDATE reports
SET    body = $1, fence_token = $2
WHERE  id = $3 AND fence_token <= $2;  -- 0 rows updated: a newer holder exists, stop
```

Use `<=`, not `<`: the same holder may write several times with the same token.

Tokens can come from ZooKeeper's zxid, etcd's revision of the lock key, or a database sequence. The
same idea appears elsewhere: Raft nodes reject messages from an older **term**, and database failover
must stop an old primary from accepting writes (see [replication](/posts/replication-and-high-availability)).

The catch: **the resource must cooperate.** A third-party API knows nothing about your tokens, so
you need an idempotency key there instead. And once the resource checks tokens, it is doing the
important concurrency control itself. That leads to the best advice in this article.

## Often you don't need a lock at all

Many "we need a lock" problems have a simpler and safer answer:

| Problem | Design without a lock |
|---|---|
| Never two payouts for one invoice | `UNIQUE (invoice_id)` and `INSERT ... ON CONFLICT DO NOTHING` |
| A retried request must not charge twice | An [idempotency key](/posts/retries-timeouts-and-idempotency) |
| Two requests change the same balance | One atomic `UPDATE ... WHERE balance >= 80`, or a version column |
| Each job processed by one worker | Claim rows with `FOR UPDATE SKIP LOCKED` |
| The nightly job must run once | The job processes "whatever is due" and marks each item done in the same atomic step, so a second run finds nothing |
| All changes to one entity in order | Single-writer partitioning |

**Single-writer partitioning** means sending all commands for account 42 to the same queue partition,
which only one consumer reads at a time (see [queues and streams](/posts/message-queues-and-event-streams)
and [partitioning](/posts/sharding-and-partitioning)). This is not magic: owning a partition works
like a lease too. A consumer that stops sending heartbeats loses its partitions, and if it was only
paused, it can wake up and finish a message that the new owner also processes. So handlers must
still be idempotent, or check a version before they write. The difference is that the queue system
manages this hand-over for you, instead of your own lock code.

These designs share one idea: **put the check where the data is**, in the same atomic step as the
change. Pauses, clocks and partitions can then delay you, but they can't make you write stale data.

## In practice

1. **Decide: efficiency or correctness?** Write the answer in a comment next to the lock.
2. **If the data lives in one database, use that database**: constraints, atomic updates,
   `FOR UPDATE`, transaction-level advisory locks.
3. **For efficiency locks**, one Redis `SET NX PX` with a random token and a safe release (the Lua
   script or `DELEX ... IFEQ`) is fine.
4. **For correctness across systems**, use a consensus-backed lease (etcd, ZooKeeper) *and* fencing
   tokens checked by the resource, or redesign so you don't need the lock.
5. **Make the lease much longer than the normal work time**, and alert when the work gets close.
6. **Make the protected work idempotent anyway.** It is your safety net when the lock fails.

## Common mistakes

- **`SETNX` then `EXPIRE` as two commands.** A crash between them leaves a lock that never expires.
  Use one `SET ... NX PX` command.
- **Releasing with a plain `DEL`**, or a fixed value instead of a random token: you may delete
  someone else's lock.
- **Auto-renewal in a background thread while the worker is stuck.** The lease never expires and
  nothing progresses. Renew only while the work really progresses.
- **Believing a lock gives "exactly once".** It reduces duplicates; only idempotency and constraints
  stop their effects.

## Further reading

- Martin Kleppmann: [How to do distributed locking](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html) (2016)
- Salvatore Sanfilippo: [Is Redlock safe?](https://antirez.com/news/101) (2016), the reply from the creator of Redis
- PostgreSQL docs: [Advisory Locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS)
- Redis docs: [SET command](https://redis.io/docs/latest/commands/set/), and the page "Distributed Locks with Redis" (linked from the SET page)
- [Apache ZooKeeper documentation](https://zookeeper.apache.org/doc/current/): the "Recipes and Solutions" page includes the lock recipe
- Martin Kleppmann, *Designing Data-Intensive Applications*: the chapter "The Trouble with Distributed Systems" (chapter 8 in the first edition, chapter 9 in the second)
