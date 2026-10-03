+++
title = "Presence at scale: showing who is online to millions of users"
summary = "How chat apps decide who is online, away or offline: heartbeats and TTLs, multiple devices, grace periods against flapping, and how lazy subscriptions, batching and sharding keep the fan-out of presence updates under control."
tags = ["realtime","scalability","system-design"]
level = "advanced"
date = 2026-10-02
+++

Open any chat app and you see a green dot next to a friend's name, "last seen 5 minutes ago", or "Ana
is typing…". It looks like the easiest feature in the product: a boolean `is_online` column. With a
thousand users, that works. With ten million people online at once, presence becomes one of the
busiest parts of the system. Every phone that locks its screen, every train that enters a tunnel and
every laptop that goes to sleep is a state change. And every change must reach everyone who might be
looking at that person.

## What "presence" means

Presence is the set of signals that tell other people whether you are available right now.

| Signal | Meaning | Lives for | Source |
|---|---|---|---|
| Online | At least one device is connected and recently used | While connected | Connections, heartbeats |
| Away (idle) | Connected, but no input for a few minutes | Minutes | The client reports it |
| Offline | No live connection | Until the next connect | Absence of signals |
| Last seen | When the user was last online | Long (stored) | Written when they go offline |
| Typing | Writing in one conversation right now | Seconds | Client events |
| Custom status, "Do not disturb" | Chosen by the user | Until changed | User settings |

A custom status is ordinary data: store it in your database like any setting. Everything else is
**derived, short-lived state**: computed from connections, out of date within seconds, and cheap to
lose because it rebuilds itself. Most of the design below follows from that.

## Where the signal comes from

**Connection-based presence.** The gateway — the server that holds the user's WebSocket (see
[scaling WebSockets](/posts/scaling-websockets-chat)) — knows when a connection opens and closes.
Open means online; close means offline. Fast and almost free. But connections often die **silently**:
a phone that loses signal sends no "goodbye" packet. And if a gateway crashes, nobody sends "closed"
events at all, so its users stay online forever: **ghost users**.

**Heartbeat-based presence.** The client (or the gateway, on its behalf) says "still here" at a fixed
interval, for example every 30 seconds, and the server stores "online until now + TTL". A **TTL** (time to live) is how long a value
stays valid before it expires automatically. If heartbeats stop for any reason, the value expires and
the user becomes offline: the system heals itself. It even works with plain HTTP polling (see
[polling, SSE and WebSockets](/posts/realtime-polling-sse-websockets)).

| | Connection-based | Heartbeat-based |
|---|---|---|
| Becomes online | Immediately | Immediately (first heartbeat) |
| Becomes offline | Immediately on a clean close; late or never on a silent drop | After the TTL |
| Gateway crash | Ghost users | Heals when TTLs expire |
| Cost | Almost nothing | One write per user per interval |

That last row matters: 10 million online users with a 30-second heartbeat is about **333,000 writes
per second**, mostly saying "nothing changed". Real systems combine both: connection events for speed,
TTLs as a safety net.

## Storing presence with TTLs

Redis (an in-memory data store, see [Redis beyond caching](/posts/redis-beyond-caching)) is a common
place to start. Store one entry **per connection**, not per user, and let entries disappear unless
someone refreshes them:

```text
# Gateway, every 30 s, for each connection it holds (pipelined, many commands per round trip)
ZADD   conns:42  1767225600  "phone-9c1e"   # member = connection id, score = last refresh (unix s)
EXPIRE conns:42  90                          # if all of user 42's devices go silent, key vanishes

# Clean disconnect (the gateway saw the socket close)
ZREM   conns:42  "phone-9c1e"

# Read "is user 42 online?"
ZREMRANGEBYSCORE conns:42  -inf  1767225510  # drop connections silent for 90 s or more
ZCARD  conns:42                              # 0 = offline, 1 or more = online
```

A **sorted set** keeps members ordered by a number, the "score". Here the score is the last refresh
time, so removing dead connections is one range delete. Use the **server's clock**, never the
client's. Make the TTL **two to three times** the refresh interval, so one lost heartbeat does not make
anyone go offline. And keep heartbeats **out of your main SQL database**: store only `last_seen` there,
written once when the user goes offline.

> [!WARNING]
> An expiring key is **silent**: nobody is told that user 42 went offline. Redis can send keyspace
> notifications for expired keys, but they are off by default, they are fire-and-forget (lost if your
> listener is disconnected), and Redis sends them only when it actually finds and deletes the expired
> key, which can be later than the moment the TTL ran out. To announce "went offline" reliably, keep
> a sorted set of `user id -> last heartbeat` (split into several sets at larger scale) and run a
> **sweeper** every few seconds that publishes offline events for users whose heartbeat is too old.

### At larger scale: one lease per gateway

Refreshing millions of entries is wasteful when the gateway already pings each client. A cheaper
design moves the heartbeat up one level:

```text
 phone --ping/pong every 30 s--> [gateway 3] --"conn opened / closed"--> [presence service]
                                      |
                                      +----"gateway 3 alive" every 5 s----> lease, TTL 15 s

 If gateway 3's lease expires, the presence service drops ALL of gateway 3's connections at once.
```

Gateways find dead clients with WebSocket ping and pong frames: the gateway sends a ping, and the
client must answer with a pong (browsers do this automatically). Each gateway also refreshes a
**lease** (a record with a short TTL); if it crashes, the lease expires and all its users are cleaned
up together. One heartbeat per **gateway**, not per user.

## Multiple devices per user

A user may have a phone, a laptop and a browser tab open at once. Their presence combines all of them:

```python
def user_state(connections):               # all live connections of one user
    if not connections:
        return "offline"
    if any(c.active for c in connections):  # input in the last few minutes
        return "online"
    return "away"                           # connected, but idle everywhere
```

- Closing **one** device never means "offline"; only closing the **last** one does. Deleting the
  user's presence when *a* connection closes is a classic bug.
- "Away" is detected by the client, which sees keyboard and touch input. The server only combines.
- To show "active on mobile", store the device type with each connection.

## Grace periods: stop the flapping

Mobile connections drop for a few seconds all the time: a lift, a tunnel, a switch from Wi-Fi to
mobile data. Without protection, friends see the user **flap** — online, offline, online — and may get
a notification each time.

```text
time (s)          0          8    12                    45
connection        ===========x    ======================
no grace period   online     OFF  online                    <- friends see a flap
20 s grace        online     (waiting...)  still online     <- nothing is published
```

The fix is an **asymmetric** rule:

1. Publish "online" **immediately**. People like a fast green dot.
2. When the **last** connection closes, schedule a check for `now + grace period` (for example 15–30
   seconds) instead of publishing "offline". A reconnect cancels the check.
3. If the user still has no connections when the check runs, publish "offline" and write `last_seen`
   with the time of the **disconnect**, not the time of the check.

Grace periods also absorb deploys: when a restarted gateway's clients reconnect within seconds,
nothing is published.

## The real cost: fan-out

Storing presence is the easy part. The expensive part is **fan-out**: delivering each change to
everyone who can see it. A quick estimate, assuming:

- 10 million users online at the same time;
- each changes visible state (connect, away, back, disconnect) about once every 5 minutes;
- each has 200 contacts, about 10% of them online at any moment.

| Quantity | Calculation | Result |
|---|---|---|
| Presence changes | 10,000,000 / 300 s | ~33,000 per second |
| Deliveries to online contacts | 33,000 × 20 | ~670,000 per second |
| One community: 100,000 members, 20,000 online, all see all | (20,000 / 300) × 20,000 | ~1,300,000 per second |

The last row is the dangerous one: **one** large room produces about twice the deliveries of the
friend lists of all 10 million users. In a room, both changes and recipients grow with the member
count, so the work grows with the **square** of the room size. And most deliveries go to people who
are not even looking at the person whose dot changed.

## Lazy presence: only what is on screen

A phone screen shows perhaps 10 to 30 people. **Subscription-based** (or **lazy**) presence uses this:
the client tells the server which users it is showing and gets updates only for them.

```text
 client shows users [42, 77, 90]               presence shard that owns user 42
       |                                      +----------------------------------+
 [gateway 3] ---- subscribe(42) ------------> | 42: online, 2 connections        |
                                              | watched by: gateway 3, gateway 7 |
 [gateway 7] ---- subscribe(42) ------------> +----------------------------------+
                                                       | 42 becomes "away"
 [gateway 3] <------- one event per gateway -----------+
 [gateway 7] <-----------------------------------------+
     each gateway forwards it to its own clients that watch 42
```

1. When a screen opens, the client subscribes to the users on it. The server replies with a
   **snapshot** of their states, then sends changes.
2. When the user scrolls or leaves the screen, the client unsubscribes. Subscriptions are tied to the
   connection, so they disappear when it or its gateway dies.
3. Each change goes **once per watching gateway**, and the gateway fans out locally — the same
   two-level fan-out used for chat messages.

For everyone else, fetch presence **on demand** with one batched read. In large rooms, show dots only
for the visible part of the member list, plus an approximate count ("1,204 online") recomputed every
few seconds.

## Batching and coalescing

Presence can be a few seconds late without anyone noticing. That allows two cheap optimisations:

- **Batching**: collect changes for 1–3 seconds and send them as one message.
- **Coalescing**: keep only the **latest** state per user in that window. Online → away → online
  within one window means nothing changed, so nothing is sent.

```python
pending = {}     # user_id -> latest state. A dict, not a list: newer changes overwrite older ones.
last_sent = {}

def on_change(user_id, state):
    pending[user_id] = state

def flush():     # called every 2 seconds
    batch = {u: s for u, s in pending.items() if last_sent.get(u) != s}
    pending.clear()
    if batch:
        publish(batch)               # one message carries many updates
        last_sent.update(batch)
```

**Typing indicators** need an even lighter path: never store them, only forward them. The client sends "typing" at most
every few seconds; receivers hide it after about 5 seconds without a new event, so a lost "stopped
typing" fixes itself. In large rooms, show "several people are typing" or turn it off.

## Sharding presence by user id

At millions of users, presence lives on many servers, called **shards**. Pick the shard from the user
id with consistent hashing, so adding a shard moves only a small part of the users (see
[sharding and partitioning](/posts/sharding-and-partitioning)).

One shard then owns everything about a user: their connections, the grace-period timer and the
gateways watching them. Combining devices is local, with no locks across servers. A read for 30 users
is split into one request per shard.

Presence can also be **rebuilt**. When a shard restarts empty, or you add shards, ask the gateways to
report their connections again; once they have all answered, the state is back, with no data migration.
One trap: a freshly started shard knows nobody, so everyone looks offline. Give it a warm-up period
during which it publishes no "offline" events.

## Privacy

Presence is personal data: a history of someone's online times shows when they sleep, work and talk.

- **Visibility settings**: let users choose who sees their online status and last seen (everyone,
  contacts, nobody). Some apps make this reciprocal: hide yours and you cannot see others'.
- **Invisible mode**: track the real state internally, but publish "offline".
- **Blocking**: a blocked user receives nothing. Check at subscribe time, and cancel subscriptions when
  the relationship changes.
- **One filter for every path**: snapshots, live updates and every read API must use the same check
  (see [authorization models](/posts/authorization-models)). Checking the API but not the push path
  is a data leak.
- **Side channels**: an invisible user shown as "typing" is not invisible.
- **Less precision**: "last seen recently" often suffices and leaks less. Keep no long history.

## Eventual consistency is fine

Presence is a **hint**, not a fact. If two friends see different states for a few seconds, nobody is
harmed. This is **eventual consistency**: when changes stop, every viewer converges to the same state
(see [CAP and consistency models](/posts/cap-theorem-and-consistency-models)).

So presence can live in memory, without transactions, and losing some updates is acceptable: a
reconnecting client asks for a fresh snapshot. For example, `Phoenix.Presence` in the Elixir web
framework Phoenix replicates presence between servers with a CRDT (a data structure that merges
updates without coordination) instead of a central database.

The flip side: never let correctness depend on presence. "Store the message only if the recipient is
online" is a bug. "Skip the push notification if the recipient seems online" is acceptable: if
presence is wrong, the worst case is one missing or one extra notification, and the message itself is
still stored and shown when the recipient opens the app.

**When you don't need all this:** with a few thousand users online, one Redis with TTL keys is
enough. If the product only needs "active in the last 15 minutes", skip real-time presence and update
a `last_active_at` column at most every few minutes.

## In practice

Starting values to tune with your own measurements — not standards:

| Setting | Starting point | Why |
|---|---|---|
| WebSocket ping interval | 30 s | Finds dead connections; shorter than common proxy idle timeouts (often 60 s by default) |
| Presence TTL or lease | 2–3 × the refresh interval | One lost heartbeat must not mean offline |
| Offline grace period | 15–30 s | Covers short network drops and reconnects |
| Away after | 5–10 min without input | A product decision |
| Batch window | 1–3 s | Nobody notices a dot that changes 2 s late |
| Typing indicator | Send at most every 3 s; hide after ~5 s | A lost "stopped typing" fixes itself |

## Common mistakes

- **`UPDATE users SET is_online = true` on every heartbeat** in the main database: heavy write load,
  and the flag stays `true` forever after a crash.
- **Relying only on close events.** Gateways crash and phones vanish silently.
- **Marking a user offline when one device disconnects** while another is still connected.
- **Pushing every change to every member** of a large room.
- **A wave of "offline" events** after a presence shard restarts empty.

## Further reading

- Phoenix documentation: [Phoenix.Presence](https://hexdocs.pm/phoenix/Phoenix.Presence.html) — presence with many connections per user, replicated without a central store
- Redis documentation: [EXPIRE](https://redis.io/docs/latest/commands/expire/) — how TTLs and key expiry work
- Redis documentation: [Sorted sets](https://redis.io/docs/latest/develop/data-types/sorted-sets/)
- RFC 6121: [XMPP Instant Messaging and Presence](https://www.rfc-editor.org/rfc/rfc6121) — a standard presence protocol in which users approve who may see their presence
- RFC 6455: [The WebSocket Protocol](https://www.rfc-editor.org/rfc/rfc6455) — includes the ping and pong frames
