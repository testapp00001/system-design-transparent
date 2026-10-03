+++
title = "Pub/sub for real-time systems: Redis Pub/Sub, Redis Streams, NATS and Kafka"
summary = "Fire-and-forget vs durable pub/sub, how Redis Pub/Sub, Redis Streams, NATS, JetStream, Kafka and MQTT really behave, and how to use one as the backplane between your WebSocket servers."
tags = ["realtime", "messaging", "distributed-systems"]
level = "intermediate"
date = 2026-10-02
+++

Your chat app runs on two servers behind a load balancer. Alice is connected to server 1, Bob to
server 2. Alice sends "hi" in a room they share. Server 1 has the message, but Bob's WebSocket lives
on server 2, which knows nothing about it. The servers need a way to broadcast to each other: a
**backplane**. The usual tool is **publish/subscribe** (pub/sub), and a search offers Redis Pub/Sub,
Redis Streams, NATS, Kafka and MQTT. They all "publish" and "subscribe", but they make very different
promises about what happens when something goes wrong.

## What pub/sub is, and the question that matters

A **publisher** sends a message to a named **topic** (Redis calls it a *channel*, NATS a *subject*).
A **broker** (the server in the middle) delivers a copy to every **subscriber** of that topic. The
publisher does not know who the subscribers are.

One question splits these systems into two families: **what happens to a message when a subscriber
is not there to receive it?**

```text
FIRE-AND-FORGET  (Redis Pub/Sub, NATS core)

  publisher --"hi"--> [ broker ] --"hi"--> subscriber A   connected: receives it
                           \
                            x              subscriber B   reconnecting: never receives it

DURABLE  (Redis Streams, JetStream, Kafka)

  publisher --"hi"--> [ broker ] --append--> log:  m1  m2  m3  hi
                                                           ^   ^
                                          subscriber B ----+   +---- subscriber A
                                          (catching up)              (up to date)
```

- **Fire-and-forget** brokers route the message to whoever is connected *right now*, then forget it.
  A subscriber that is offline, slow or reconnecting misses it. This is **at-most-once** delivery:
  once or not at all. In exchange, latency is very low and the broker keeps almost no state.
- **Durable** brokers first store the message in a log. Subscribers read at their own pace,
  **acknowledge** (confirm) what they processed, and can **replay** (read again) old messages. This
  is usually **at-least-once** delivery: after a crash before the acknowledgement, a message can
  arrive twice, so consumers must handle duplicates.

Fire-and-forget is live radio; durable is a podcast that waits for you.

> [!TIP]
> Ask: "Is this message the source of truth, or only a notification?" If the real data is safely in
> a database, fire-and-forget is often enough. If the message *is* the data (an order, a payment),
> it needs a durable system. More in [message queues vs event streams](/posts/message-queues-and-event-streams).

## Redis Pub/Sub: the simplest backplane

Many teams already run Redis, so this is a natural first try:

```text
SUBSCRIBE room:42                       # connection 1: one channel
PSUBSCRIBE room:*                       # connection 2: pattern subscription (glob-style)

PUBLISH room:42 "{\"text\":\"hi\"}"     # connection 3
(integer) 2                             # number of clients that received it
```

Inside Redis there is no queue. The server keeps a map from channel to subscribed connections.
`PUBLISH` copies the message into the output buffer of each subscriber and forgets it. So:

- **No persistence.** Nothing is stored; after a restart there is nothing to recover.
- **At-most-once.** The Redis documentation says so: if a subscriber cannot handle a message (an
  error, a disconnect), the message is lost.
- **Subscribers must be connected.** A gateway that reconnects after a two-second network problem
  has missed those two seconds, with no warning.
- **No acknowledgements, no load balancing.** Every subscriber gets every message.
- **Slow subscribers are disconnected.** The default `client-output-buffer-limit pubsub 32mb 8mb 60`
  closes a subscriber whose buffer passes 32 MB, or stays above 8 MB for 60 seconds. Your gateway must
  notice, reconnect and resubscribe.

### Redis Cluster and sharded pub/sub

In Redis Cluster, classic `PUBLISH` is a broadcast: every message goes to **every node**, because a
subscriber could be connected to any of them. Adding nodes does not add pub/sub capacity; it only
adds more nodes that must handle every message.

**Redis 7.0 added sharded pub/sub.** `SSUBSCRIBE` and `SPUBLISH` hash the channel name to a slot,
like a key. The message stays inside the shard that owns the slot (its primary and replicas), so
capacity grows with the number of shards. The costs: there are no pattern subscriptions, a
subscriber must connect to a node of the right shard (cluster-aware client libraries do this), and
all channels in one `SSUBSCRIBE` call must hash to the same slot.

## Redis Streams: a durable log inside Redis

Redis Streams (since Redis 5.0) is a different data type with a different contract: an append-only
log stored under a key. Each entry gets an ID made of a millisecond timestamp and a sequence number.
Entries stay until you trim them.

```text
XGROUP CREATE orders billing $ MKSTREAM                   # group reads entries added from now on

XADD orders MAXLEN ~ 100000 * order_id 1042 status paid   # append, keep ~100k entries
"1727870000000-0"

# worker-1 reads up to 10 new entries, waiting up to 5 s
XREADGROUP GROUP billing worker-1 COUNT 10 BLOCK 5000 STREAMS orders >
XACK orders billing 1727870000000-0                       # done with this entry

# take over entries another worker left unacknowledged for 60 s (Redis 6.2+)
XAUTOCLAIM orders billing worker-2 60000 0-0 COUNT 10
```

- **Consumer groups.** Each entry goes to one worker in a group; every group gets every entry.
- **Acknowledgements.** Delivered but unacknowledged entries wait in the group's **pending entries
  list**. If a worker crashes, another claims them. This is at-least-once delivery.
- **Replay.** Clients can read history with `XRANGE`, as long as it was not trimmed.

The limits come from Redis itself:

- **Memory.** The stream lives in RAM. Always trim (`MAXLEN` or `MINID`).
- **Durability is your Redis configuration.** Streams reach disk only through RDB snapshots or the
  AOF log (AOF is off by default). With AOF and `appendfsync everysec`, a crash can lose about the
  last second of writes, and asynchronous replication means a failover can lose recent entries too.
- **One stream lives on one shard**, because it is one key. Split hot streams yourself.

Streams fit modest durable queues when you already run Redis. They are awkward as a live backplane
for thousands of rooms: a gateway would have to block-read thousands of keys. See also
[Redis beyond caching](/posts/redis-beyond-caching).

## NATS: subjects, wildcards and queue groups

NATS is a messaging system built for this work: a small server written in Go. Core NATS is
fire-and-forget like Redis Pub/Sub, with richer routing.

**Subjects** are tokens separated by dots, such as `chat.room.42`. Subscribers can use wildcards:
`*` matches exactly one token (`chat.room.*`), and `>` matches one or more tokens at the end
(`chat.>` also matches `chat.room.42.typing`).

**Queue groups** add load balancing with no server configuration: subscribers with the same queue
name share the messages, each going to one randomly chosen member. Core NATS also has built-in
**request/reply**.

```text
nats sub 'chat.room.*'                     # every message for every room
nats sub --queue indexers 'chat.room.*'    # run 3 of these: each message goes to one
nats pub chat.room.42 'hi'
```

Delivery is **at-most-once**: the NATS documentation states that a message is not received if no
subscriber is listening or active at that moment. In a NATS cluster, servers share which subjects
their clients want and forward messages only where there is interest. As in Redis, the server
disconnects a **slow consumer** (a subscriber that cannot keep up); client libraries can also drop
messages when their local buffer is full.

### JetStream: persistence and replay

**JetStream** is the persistence layer built into the NATS server (`nats-server -js`). It adds:

- **Streams** that store every message on a set of subjects (for example `orders.>`), in files or
  memory, with up to 5 replicas kept consistent by Raft (see
  [consensus and leader election](/posts/consensus-and-leader-election)).
- **Consumers**: durable positions in a stream with acknowledgements; unacknowledged messages are
  redelivered.
- **Replay** from the start, a sequence number, a point in time, or the last message per subject.
- **Retention** by limits (age, count, size), by consumer *interest*, or as a *work queue*, plus
  per-subject limits such as "keep the last 100 messages of each room".
- **Deduplication** of publishes with the same `Nats-Msg-Id` header within a time window.

One caveat: the NATS documentation explains that, by default, file-based streams are not `fsync`ed
to disk after every message; the server syncs on an interval (`sync_interval`, two minutes by
default). Replication protects
you against most failures; if you need every message on disk before the acknowledgement, set
`sync_interval: always` and accept slower writes.

The attraction is one system for both lossy real-time fan-out and durable streams, on the same
subjects.

## Kafka: the durable, high-throughput log

Apache Kafka is a distributed, replicated log. A topic is split into **partitions**, stored on disk
and copied to several brokers. Consumers in a **consumer group** split the partitions and track
their **offset** (position). Messages are kept for a retention period (7 days by default) whether
anyone read them or not. It is the usual choice for a high-volume event backbone shared by many
teams.

It is a poor fit for the last hop to WebSocket servers:

- **No cheap per-room subscription.** Consumers read whole partitions, so a gateway reads every room
  in the partition room 42 hashes to and throws most of it away.
- **Topics are heavy.** Partitions are files on every replica plus cluster metadata; one topic per
  room does not work with millions of rooms.

A common combination: Kafka stores the events, and a fan-out service republishes them to Redis
Pub/Sub or NATS for the real-time last hop.

## MQTT: pub/sub for devices

MQTT is an open pub/sub **protocol** (3.1.1 and 5.0 are OASIS standards) designed for small devices
on unreliable, low-bandwidth networks. Brokers include Eclipse Mosquitto, EMQX, HiveMQ and VerneMQ.

- Topics use `/` levels, with wildcards `+` (one level) and `#` (the rest): `home/+/temperature`.
- **QoS** (quality of service): 0 = at most once, 1 = at least once, 2 = exactly once, between one
  client and the broker, not end to end.
- **Retained messages** give new subscribers the last value at once; a **last will** message is
  published when a device disconnects unexpectedly; **persistent sessions** keep QoS 1 and 2
  messages for offline devices.

## Pub/sub as the backplane between WebSocket gateways

Back to chat. Each **gateway** (a server holding WebSocket connections) subscribes to the topics of
the rooms its users are in. The message service saves a new message, then publishes it once. The
broker delivers one copy per interested gateway, and each gateway writes it to its local sockets.

```text
  users       o o o             o o              o o o o
               \|/              \|/                \|/
          [ gateway 1 ]     [ gateway 2 ]      [ gateway 3 ]
          rooms: 42, 7      rooms: 42          rooms: 7, 99
                ^                 ^             (not subscribed to room.42:
                |                 |              receives nothing)
                +--------+--------+
                         |
          [ broker: Redis Pub/Sub or NATS ]
                         ^
                         |  publish room.42 "hi"   (after saving it to the database)
                 [ message service ]
```

This is the *two-level fan-out* from [scaling WebSockets](/posts/scaling-websockets-chat). A gateway
subscribes when its first local user joins a room and unsubscribes when the last one leaves:

```python
from collections import defaultdict

members = defaultdict(set)        # room_id -> local connections
subscribed = set()                # rooms this gateway is subscribed to

async def join(conn, room_id):
    members[room_id].add(conn)
    if room_id not in subscribed:  # first local member (or the grace period already ended)
        subscribed.add(room_id)
        await broker.subscribe(f"room.{room_id}")

async def leave(conn, room_id):
    members[room_id].discard(conn)
    if not members[room_id]:
        # after 30 s: if the room is still empty, unsubscribe and remove it from `subscribed`
        schedule_unsubscribe(room_id, after_seconds=30)

async def on_broker_message(room_id, payload):
    frame = encode_websocket_frame(payload)   # encode once, send many
    for conn in members.get(room_id, ()):
        conn.try_send(frame)                  # bounded buffer per connection
```

### Subscription churn

**Subscription churn** is subscribe and unsubscribe commands arriving at a high rate. In production,
every room join, page refresh and mobile network change produces broker commands, and a NATS cluster
also shares that interest between servers. The worst moment is a **gateway restart**: its users
reconnect elsewhere within seconds, and other gateways subscribe to thousands of topics at once. To
control it:

- **Delay unsubscribes** by a grace period: users often come back within seconds.
- **Batch subscriptions**: `SUBSCRIBE` accepts many channels in one command (for `SSUBSCRIBE`, only
  channels in the same slot).
- **Reconnect clients with backoff and jitter** (see [retries and timeouts](/posts/retries-timeouts-and-idempotency)).
- **Choose topic granularity on purpose:**

| Topic design | Subscriptions per gateway | Churn | Trade-off |
|---|---|---|---|
| Per room: `room.42` | One per room with local users | High | Broker tracks interest for you |
| Buckets: `rooms.17` (room id hash mod N) | At most N | Low | Gateways also get other rooms in the bucket |
| Per gateway: `gateway.3` | One | None | You maintain a "room → gateways" registry, which churns instead |
| Per user: `user.123` | One per connected user | Very high | Good for private notifications, not big rooms |

Presence (who is online) has a similar churn problem; see [presence at scale](/posts/presence-at-scale).

### A lossy backplane is fine if you saved first

With Redis Pub/Sub or core NATS, a gateway that is reconnecting loses messages. That is acceptable
**only if** each message was saved first with a per-room sequence number. Clients remember the last
number they saw and, after a gap or reconnect, fetch what they missed through a normal API. After a
broker disconnect, a gateway must resubscribe to all its topics and tell its clients to catch up.

## Comparison

| | Redis Pub/Sub | Redis Streams | NATS core | JetStream | Kafka | MQTT broker |
|---|---|---|---|---|---|---|
| Stores messages | No | RAM + Redis persistence | No | Files or memory, replicated | Disk, replicated | Retained value, offline sessions |
| Delivery | At-most-once | At-least-once | At-most-once | At-least-once | At-least-once | QoS 0, 1 or 2 |
| Replay | No | Until trimmed | No | By sequence or time | By offset or time | No |
| Load-balanced consumers | No | Consumer groups | Queue groups | Consumers | Consumer groups | Shared subscriptions (MQTT 5) |
| Wildcards | Patterns (not sharded) | No | `*` and `>` | `*` and `>` | Regex on topic names | `+` and `#` |
| Cost of a new topic | Almost zero | One key | Almost zero | Subjects cheap, streams heavier | High | Almost zero |

## How to choose

- **You run Redis, have a few gateways, and save messages in a database first** → Redis Pub/Sub
  (sharded on Redis Cluster).
- **You need a small durable queue and already run Redis** → Redis Streams, trimmed and monitored.
- **Many services, wildcards, request/reply, durability where needed** → NATS plus JetStream.
- **Many teams, high volume, days of retention and replay** → Kafka.
- **The clients are devices** → an MQTT broker, bridged into the backend.
- **One small app on Postgres** → `LISTEN/NOTIFY` may be enough before adding any broker. It is
  also fire-and-forget: only sessions listening at that moment get the notification.

## Common mistakes

- **Fire-and-forget for data that must not be lost.** Use a durable system, with idempotent
  consumers because it will redeliver.
- **Publishing before saving**, or treating the backplane as the source of truth. If you can crash
  between saving and publishing, use an [outbox](/posts/distributed-transactions-saga-outbox) or
  client catch-up.
- **Classic `PUBLISH` on a large Redis Cluster.** Every node handles every message; use `SPUBLISH`.
- **One Kafka topic per room or user.** Use keys and partitions.
- **Ignoring broker disconnects.** A gateway that does not resubscribe silently stops delivering.
- **Never trimming or acknowledging a stream** until memory runs out.

## Further reading

- Redis docs: [Redis Pub/Sub](https://redis.io/docs/latest/develop/pubsub/)
- Redis docs: [Redis Streams](https://redis.io/docs/latest/develop/data-types/streams/)
- NATS docs: [Subject-based messaging](https://docs.nats.io/nats-concepts/subjects) and [JetStream](https://docs.nats.io/nats-concepts/jetstream)
- [Apache Kafka documentation: Design](https://kafka.apache.org/documentation/#design)
- OASIS: [MQTT Version 5.0](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html)
