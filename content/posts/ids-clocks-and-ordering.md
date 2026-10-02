+++
title = "Time and ordering: clocks, Lamport timestamps, Snowflake IDs and UUIDv7"
summary = "Why server clocks drift and jump, how logical clocks, hybrid clocks and TrueTime order events safely, and how to choose between auto-increment, UUIDv4, UUIDv7, ULID and Snowflake IDs."
tags = ["distributed-systems","database"]
level = "advanced"
date = 2026-10-02
+++

A customer changes her delivery address on her phone, then fixes a typo on her laptop a second
later. The two requests reach two different app servers. Each server stamps its write with its own
clock, and the database keeps the newest timestamp. Server B's clock is two seconds slow, so the typo
wins. Nothing crashes, nothing is logged, and the parcel goes to the wrong address.

Distributed systems keep asking two questions: **what happened first?** and **how do I give each thing
a unique name?** "Use the time" is the obvious answer to both. This article shows why clocks are
weaker than they look, what to use instead, and how ID formats use time safely.

## Wall clocks drift and jump

The **wall clock** (`CLOCK_REALTIME` on Linux) gives the time of day. It is wrong in several ways:

- **Drift.** A quartz crystal drives the clock, and its speed varies with temperature. An error of
  20 parts per million (ppm) adds up to about 1.7 seconds per day.
- **NTP corrections.** NTP (Network Time Protocol) daemons such as `chronyd` fix small errors by
  **slewing** (running the clock slightly faster or slower) and large errors by **stepping**: the
  clock jumps, forwards or **backwards**.
- **Limited accuracy.** NTP typically keeps clocks within tens of milliseconds over the public
  internet, and can do better than a millisecond on a good local network. A server whose NTP
  silently broke, or a paused virtual machine, can be off by much more.

### Leap seconds and leap smearing

UTC sometimes adds a **leap second** (`23:59:60`) to follow the Earth's irregular rotation. Unix time
cannot represent it, so systems usually repeat a second or step back by one. Cloudflare has described
how the leap second at the end of 2016 produced a negative time difference in its DNS software
(code that assumed time never goes backwards), so some DNS lookups failed. Some large operators,
such as Google, use **leap smearing** instead: their time servers spread the extra second over many
hours (Google documents a 24-hour linear smear, from noon to noon UTC), so clocks never jump. Google
also advises not to mix smeared and non-smeared time servers: during the smear they disagree by up
to half a second. Pick one kind for the whole fleet.

> [!NOTE]
> In 2022 the General Conference on Weights and Measures (CGPM) decided to let UTC drift further
> from the Earth's rotation by 2035 at the latest. In practice this ends leap seconds. Clocks will
> still step backwards for other reasons.

### Measure durations with a monotonic clock

The **monotonic clock** (`CLOCK_MONOTONIC`) never goes backwards. Its value means nothing on its own
and can't be compared between machines, but it is the right tool for "how long did this take?".

| Language | Wall clock (time of day) | Monotonic clock (durations) |
|---|---|---|
| Python | `time.time()` | `time.monotonic()` |
| Java | `System.currentTimeMillis()` | `System.nanoTime()` |
| Go | `time.Now()` | `time.Since(start)` (uses a monotonic reading) |
| Rust | `SystemTime::now()` | `Instant::now()` |
| JavaScript | `Date.now()` | `performance.now()` |

Rule of thumb: the wall clock is for **timestamps you store or show**. The monotonic clock is for
**durations, timeouts, rate limits and leases** (see [distributed locks](/posts/distributed-locks)).

## Why "the newest timestamp wins" loses data

**Last-write-wins (LWW)** keeps whichever of two conflicting writes has the higher timestamp.
Cassandra resolves conflicting writes to the same column this way, as do many home-made sync
features. The opening story:

```text
phone  -> server A (clock correct)   "12 Mian St" (typo)  stamped 09:00:01.000
laptop -> server B (clock 2 s slow)  "12 Main St" (fix)   stamped 09:00:00.000   (sent 1 s later)

database: 09:00:01.000 > 09:00:00.000  -> keeps "12 Mian St", silently drops the newer fix
```

With **clock skew** (the difference between two machines' clocks), a later write can carry a smaller
timestamp and disappear without any error. Even with perfect clocks, LWW drops one of two concurrent
writes by design. That is fine for a cache entry or "last seen at". For data people care about:

- **Version numbers (compare-and-set):** `UPDATE ... SET ..., version = version + 1 WHERE id = $1 AND
  version = $2`. The second writer gets a conflict instead of a silent loss (see
  [transactions and isolation levels](/posts/transactions-and-isolation-levels)).
- **One owner per record:** a single leader orders all writes for a key in its log (see
  [consensus and leader election](/posts/consensus-and-leader-election)).
- **Merge instead of choose:** keep both versions and merge them, or use data types that merge
  automatically ([CRDTs](/posts/collaborative-editing-ot-crdt)).

## Logical clocks: order without time

In 1978 Leslie Lamport pointed out that we usually don't need the real time. We need to know whether
one event could have **influenced** another.

Event `a` **happens before** event `b` (written `a → b`) if both are in the same process and `a` comes
first, or `a` sends a message that `b` receives, or there is a chain `a → x → b`. If neither `a → b`
nor `b → a`, the events are **concurrent**: not "at the same instant", but "neither could have known
about the other".

### Lamport timestamps

Each process keeps a counter. It adds 1 before each local event or send, and attaches the counter to
every message. On receive, it sets the counter to `max(own, received) + 1`.

```text
time    process A                 process B                     process C
 |      a1  L=1                                                 c1  L=1
 |      a2  L=2  --- message -->  b1  L=max(0,2)+1 = 3
 |      a3  L=3                   b2  L=4  --- message ------>  c2  L=max(1,4)+1 = 5
 v
```

The guarantee: **if `a → b`, then `L(a) < L(b)`**. Along the chain a1 → a2 → b1 → b2 → c2 the numbers
always grow. Compare the pair `(L, process_id)` and you get a **total order** that every node agrees
on.

The reverse is not true. `L(a3) = 3 < L(b2) = 4`, yet a3 and b2 are concurrent. A smaller Lamport
timestamp does not prove that an event came first, and it says nothing about real time.

### Vector clocks, briefly

A **vector clock** keeps one counter **per process**, such as `[A:2, B:4, C:0]`. A process adds 1 to
its own entry for each local event or send. On receive, it takes the element-wise maximum of the two
vectors, then increments its own entry. If every entry of X is ≤ the matching entry of Y (and
X ≠ Y), X happened before Y. If some are bigger and some smaller (`[A:2, B:0]` vs `[A:1, B:1]`), the
events are **concurrent**: a real conflict. Amazon's 2007 Dynamo paper used vector clocks to detect
such conflicts and let the application merge the versions. The cost is one entry per writer.

## Physical time with error bars: HLC and TrueTime

Databases that offer snapshots ("read everything as of 09:00:00") or a global order of transactions
need causal order **and** real time.

### Hybrid logical clocks (CockroachDB)

A **hybrid logical clock** (HLC) is a pair `(wall, logical)`. `wall` is the largest physical time the
node has seen, from its own clock **or from any message it received**. `logical` is a counter that
breaks ties while `wall` doesn't advance. This keeps the Lamport guarantee and stays close to real
time.

CockroachDB uses HLC timestamps for transactions. It relies on a configured **maximum clock offset**
between nodes (the `--max-offset` start flag, 500 ms by default in its documentation). When a read
finds a value stamped slightly after its own timestamp, but within that offset, it can't tell which
came first, so it restarts at a later timestamp (an "uncertainty restart"). Because correctness
depends on that bound, a node shuts itself down if its clock is too far from the others.

### Spanner's TrueTime

Google's Spanner makes clock error small and **known** instead. Its data centers have time servers
with GPS receivers and atomic clocks. The TrueTime API returns an interval,
`TT.now() = [earliest, latest]`, guaranteed to contain the true time. The 2012 Spanner paper reported
an uncertainty of a few milliseconds. On commit, Spanner waits it out:

```text
commit:  s = TT.now().latest                  choose the commit timestamp
         wait until TT.now().earliest > s     "commit wait": s is now surely in the past
         make the write visible
=> any transaction that starts later, on any machine, gets a larger timestamp
```

Spanner pays a few milliseconds per write and special hardware. CockroachDB runs on ordinary
NTP-synchronised servers and pays with occasional restarts. The shared lesson: **you can order events
by time only if you know how wrong your clock may be.**

## Generating IDs

A good ID is unique, compact and cheap to generate. Ideally it sorts by creation time, so B-tree
indexes insert at the end instead of at random places (see
[how databases store data](/posts/how-databases-store-data)).

**Auto-increment** (`GENERATED ALWAYS AS IDENTITY` in PostgreSQL, `AUTO_INCREMENT` in MySQL) on a
`BIGINT` column is 8 bytes and appends to the index. But the database must hand out every ID, which
gets hard across shards (see [sharding](/posts/sharding-and-partitioning)). IDs reveal volume and
invite enumeration (`/invoices/1001`, `/invoices/1002`). And values are assigned at insert time, not commit time, so a
larger ID can become visible **before** a smaller one.

**UUIDv4** is 128 bits, 122 of them random, generated anywhere without coordination. A 50% chance of
a single collision needs about 2.7 × 10^18 IDs, so with a good random generator collisions are not a
practical worry. The costs: 16 bytes and **random order**, so each insert lands at a random spot in
the index (see [LSM trees vs B-trees](/posts/lsm-trees-vs-b-trees)).

### UUIDv7 (RFC 9562)

RFC 9562 (May 2024) replaced the old UUID standard, RFC 4122. It added versions 6, 7 and 8. Version
7 puts a timestamp first:

```text
+-------------------------+---------+-------------+---------+-------------+
| unix_ts_ms (48 bits)    | ver (4) | rand_a (12) | var (2) | rand_b (62) |
| ms since 1970-01-01 UTC | 0111    | random/ctr  | 10      | random      |
+-------------------------+---------+-------------+---------+-------------+

01a0fbd7-8280-7c41-9d3e-0f8a6b2c4d1e    first 12 hex digits: 2026-10-02 09:00:00 UTC
```

It fits existing `uuid` columns and libraries, and new IDs sort roughly by creation time. The RFC
allows using the `rand_a` bits for extra clock precision or for a counter, so one generator can keep
its IDs strictly increasing within a millisecond. IDs from different generators are still only
roughly ordered. PostgreSQL 18 added a built-in `uuidv7()` function.

**ULID** is an older community specification with nearly the same layout (48-bit millisecond
timestamp, 80 random bits), written as 26 Crockford Base32 characters (`01M3XXF0M04TFF59TDWH9EDD1R`).
Prefer UUIDv7 for new systems: it is an IETF standard in the UUID format that tools already know.

### Snowflake

Twitter announced Snowflake in 2010. Its README explains that Twitter was moving from MySQL towards
Cassandra and needed a new way to create IDs (for example, tweet IDs) on many machines, without a
central database. A Snowflake ID is a 64-bit integer:

```text
+---+------------------------------------------+-----------------+---------------+
| 0 | timestamp (41 bits)                      | machine id (10) | sequence (12) |
|   | ms since a custom epoch, about 69 years  | 1,024 machines  | 4,096 per ms  |
+---+------------------------------------------+-----------------+---------------+
```

```python
EPOCH_MS = 1_600_000_000_000          # your own custom epoch (here 2020-09-13)

def snowflake(now_ms: int, machine_id: int, sequence: int) -> int:
    return ((now_ms - EPOCH_MS) << 22) | (machine_id << 12) | sequence
```

Within one millisecond the generator increments the sequence; after 4,096 IDs it waits for the next
millisecond. Twitter's original README says it refuses to generate IDs while the clock runs
backwards.

The ID fits a `BIGINT`. The price: every generator needs a **unique machine ID**
(from configuration, a coordination service such as etcd, or a Kubernetes StatefulSet's pod number),
and the ID reveals when and roughly where it was made. Discord's API documents its own variant (see
the [Discord case study](/posts/discord-message-storage-case-study)).

| Scheme | Size | Sorted by time? | Generated by | Reveals |
|---|---|---|---|---|
| Auto-increment | 8 bytes (`BIGINT`) | Insert order, not commit order | The database | Row counts |
| UUIDv4 | 16 bytes | No | Anyone | Nothing |
| UUIDv7 / ULID | 16 bytes | Roughly (ms) | Anyone | Creation time |
| Snowflake | 8 bytes | Roughly (ms) | Generators with unique machine IDs | Time, machine |

## Clock skew and time-ordered IDs

Time-ordered IDs inherit every clock problem above:

- **Order across machines is approximate.** If server A runs 50 ms ahead, its IDs sort after IDs that
  server B creates up to 50 ms later. Fine for index locality and "newest first" lists; not proof of
  which event came first.
- **The clock steps backwards.** A generator that restarts, forgets its last timestamp and finds the
  clock behind can reuse old timestamps: for Snowflake, that means **duplicate IDs**. UUIDv7 and
  ULID have enough random bits to make duplicates very unlikely, but ordering breaks. Persist the
  last timestamp, or take a fresh machine ID at start-up.
- **In-flight transactions.** The ID is created before the row commits, so a slow transaction can
  commit an *older* ID after newer rows are visible. A consumer polling `WHERE id > last_seen_id`
  skips that row forever. To see every change in order, read a real log (see
  [change data capture](/posts/change-data-capture)).

Monitor clock offset like you monitor disk space; `chronyc tracking` shows the current offset.

## Choosing an ID scheme

1. **One database, internal use:** a `BIGINT` identity column. If IDs appear in URLs, add a separate
   UUID column as the public ID.
2. **IDs created outside the database** (clients, offline apps, several services, merged databases):
   **UUIDv7**, or **UUIDv4** when the creation time must stay private.
3. **Very high write rates and a need for 64-bit integers:** a **Snowflake**-style scheme, if you can
   manage machine IDs and watch your clocks.
4. **Secrets** (session IDs, password-reset links, API keys): none of the above. Use at least 128 bits
   from a cryptographically secure generator, such as Python's `secrets.token_urlsafe(32)`. RFC 9562
   itself warns against assuming UUIDs are hard to guess.
5. **Ordering events:** don't derive it from timestamps or IDs made on different machines. Get it from
   one sequencer: a per-record version, a Kafka partition offset, a consensus log, or the commit
   log of one database (read through change data capture).

## Common mistakes

- **Measuring timeouts or latency with the wall clock**, and **last-write-wins** for important data.
- **Trusting `created_at` order.** In PostgreSQL, `now()` returns the *start* time of the transaction,
  not the commit time.
- **Sending 64-bit IDs as JSON numbers.** JavaScript numbers hold integers exactly only up to 2^53,
  and a Snowflake ID passes 2^53 about 25 days after its epoch. `JSON.parse` then silently rounds
  the last digits. Send them as strings, as Twitter's (`id_str`) and Discord's APIs do.
- **Storing UUIDs as `CHAR(36)` text.** Use `uuid` in PostgreSQL or `BINARY(16)` in MySQL.
- **Two generators with the same machine ID**, for example after copying a config file.

## Further reading

- Leslie Lamport, *Time, Clocks, and the Ordering of Events in a Distributed System* (1978), and
  Corbett et al., *Spanner: Google's Globally-Distributed Database* (OSDI 2012).
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapter "The Trouble with Distributed
  Systems" (chapter 8 in the first edition).
- RFC 9562: [Universally Unique IDentifiers (UUIDs)](https://www.rfc-editor.org/rfc/rfc9562)
- [ULID specification](https://github.com/ulid/spec) and
  [twitter-archive/snowflake](https://github.com/twitter-archive/snowflake) (the `snowflake-2010` tag
  has the original README)
- Google: [Leap Smear](https://developers.google.com/time/smear)
- Cloudflare: [How and why the leap second affected Cloudflare DNS](https://blog.cloudflare.com/how-and-why-the-leap-second-affected-cloudflare-dns/)
