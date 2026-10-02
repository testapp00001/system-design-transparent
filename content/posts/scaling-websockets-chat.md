+++
title = "Scaling WebSockets: designing a chat room for 10,000 people"
summary = "Why a big chat room is a fan-out problem, how gateways and pub/sub split the work, and the concrete optimisations — batching, encode-once, backpressure, lazy presence — for throughput-first vs latency-first requirements."
tags = ["realtime", "scalability", "system-design", "networking"]
level = "advanced"
date = 2026-10-02
+++

A chat app with rooms of 20 people runs happily on one server with a WebSocket library and an
in-memory map of `room -> connections`. Then a streamer opens a room for 10,000 fans and everything
falls over. Not because of the connections — because of the **messages**.

This article walks through the problem the way you would in a design review: numbers first, then
architecture, then the optimisations that matter, and how the answer changes when you care more about
throughput than latency.

## Two different problems

1. **Holding connections.** 10,000 open WebSockets. This is the part people worry about, and it's the
   easy part: an idle WebSocket is a TCP connection plus some buffers. A single well-tuned server can
   hold tens or hundreds of thousands of mostly idle connections.
2. **Fan-out.** Every message sent to the room must be delivered to *every* member. One message in,
   10,000 messages out. This is the hard part.

## Do the math first

Assume each message is about 300 bytes on the wire.

| Scenario | Messages in/s | Deliveries out/s | Outbound bandwidth |
|---|---|---|---|
| Quiet: 1% of members send 1 msg/min | ~1.7 | ~17,000 | ~5 MB/s |
| Busy: 10% send 1 msg / 30 s | ~33 | ~333,000 | ~100 MB/s (~0.8 Gbit/s) |
| Chaos: everyone sends 1 msg / 10 s | 1,000 | 10,000,000 | ~3 GB/s |

Deliveries = (message rate) × (members). And since the message rate *also* grows with the number of
members, total work grows roughly with the **square** of the room size. Doubling a busy room
quadruples the load.

Notice something about the last row: no human can read 1,000 messages per second. At that scale the
problem is partly a **product** problem — which is why big platforms have "slow mode", follower-only
chat and other ways to limit how fast a huge room can move. Good system design questions the
requirement before scaling it.

## Architecture: gateways + a fan-out layer

One server can't hold all users and all rooms forever, so split responsibilities:

```text
            clients (10,000 in room R, spread over gateways)
   o o o o o o o o o o o o o o o o o o o o o o o o o o o o o o o
     \  |  /         \  |  /          \  |  /          \  |  /
   [gateway 1]     [gateway 2]      [gateway 3]  ...  [gateway N]     hold sockets, auth, heartbeats
        ^               ^                ^                 ^
        |  1 copy each  |                |                 |
        +-------+-------+--------+-------+--------+--------+
                |                         |
          [ room R's fan-out ]  <---- [message service] <---- POST "hi" (persisted first)
          (pub/sub topic or room server)
```

- **Gateways** (connection servers) terminate WebSockets, authenticate, keep heartbeats, and hold an
  in-memory map `room -> local connections`. They are otherwise stateless and can be added freely.
- **The message service** validates a new message, **stores it first** (database or log) with a
  per-room sequence number, then publishes it.
- **The fan-out layer** delivers the message **once per gateway** that has members of the room, not
  once per member. Each gateway then loops over its local connections.

With 10,000 members spread over 20 gateways, publishing costs 20 network sends instead of 10,000.
This *two-level fan-out* is the single most important idea in this article.

Two common ways to build the fan-out layer:

- **Pub/sub broker** (Redis Pub/Sub, NATS, a Kafka topic per shard): a gateway subscribes to `room:R`
  when its first member joins R and unsubscribes when the last leaves. The broker sends each message
  once per subscribed gateway.
- **Room servers**: rooms are assigned to servers by consistent hashing of the room id. The room
  server knows which gateways hold its members and sends to them directly. Slack has described an
  architecture along these lines, with channel servers that own channels by consistent hashing and
  gateway servers that hold client connections. Discord runs each server ("guild") as an Erlang/Elixir
  process and open-sourced *Manifold*, a library that groups recipients by node so a message is sent
  across the network at most once per node.

## Optimisations that actually matter

### Encode once, send many

Serialise the message to bytes (and build the WebSocket frame) **once**, then write the same buffer to
every connection. Serialising JSON 10,000 times per message is pure waste.

Per-message compression (`permessage-deflate`) is the opposite trap: it compresses separately per
connection, multiplying CPU by the number of recipients. For broadcast-heavy traffic, disable it or
send pre-compressed payloads.

### Batch (when throughput matters more than latency)

Instead of one frame per message per user, a gateway can collect messages for a room for a short
window — say 50–200 ms — and send them as one frame. With 30 messages/s in a room and a 100 ms window,
each connection receives 10 frames/s instead of 30, cutting syscalls and per-frame overhead by 3× at
the cost of up to 100 ms extra latency. Humans don't notice 100 ms in chat.

### Throttle the noisy, non-essential events

In big rooms, the expensive traffic often isn't messages:

- **Typing indicators**: don't broadcast "X is typing" for 10,000 people. Aggregate ("several people
  are typing") or disable above a room size.
- **Presence** (online/offline): with 10,000 members, someone joins or leaves every second. Don't push
  every change to everyone. Clients can subscribe only to the part of the member list that is on
  screen and fetch the rest on demand.
- **Read receipts** per message don't make sense in a 10,000-person room; drop them.

### Backpressure: protect yourself from slow clients

A user on a bad mobile connection reads slower than the room writes. If the gateway queues messages
for them without limit, memory grows until the process dies — taking 10,000 healthy connections with
it. Give each connection a **bounded send buffer**. When it's full: drop non-essential events, then
skip to the latest messages, and finally disconnect the client (it will reconnect and catch up from
storage).

### Rate-limit senders

Per-user and per-room rate limits (and "slow mode") cap the input side of the equation, which caps
the output side too. See [rate limiting](/posts/rate-limiting).

## Reliability: the socket is not the source of truth

Connections drop constantly: phones switch networks, laptops sleep, gateways get deployed. Design for
it:

1. **Persist before publishing.** Every message gets a monotonically increasing sequence number within
   its room.
2. **Clients track the last sequence they saw.** On reconnect they ask "give me everything after 1042"
   via a normal API call, then resume live delivery.
3. **At-least-once + dedupe.** Gaps are filled by refetching; duplicates are dropped by message id.
4. **Heartbeats.** Ping/pong every ~30 s detects dead connections (and keeps load balancers and NATs,
   which often close idle connections after ~60 s, from cutting you off).

With this, the real-time layer can be *lossy* without the product losing messages — which makes the
whole system much easier to operate.

## Operating many long-lived connections

- **File descriptors:** each socket is one. Raise `ulimit -n` / `LimitNOFILE` well above the expected
  connection count.
- **Memory per connection:** kernel socket buffers plus your per-connection state. Measure with
  realistic, mostly idle clients.
- **Load balancers:** they must support the HTTP Upgrade, and their idle timeout must be longer than
  your heartbeat interval. Because connections last for hours, load is balanced at *connect* time only;
  a newly added gateway stays empty until clients reconnect.
- **Deploys and reconnect storms:** restarting a gateway disconnects all of its clients at once. Drain
  gateways gradually, and have clients reconnect with **exponential backoff and random jitter** so
  10,000 reconnections are spread over seconds, not milliseconds.

## Throughput-first vs latency-first

| Decision | Latency-first (fast, small chats, games) | Throughput-first (huge rooms, broadcasts) |
|---|---|---|
| Sending | Immediately, one frame per message | Batch per room every 50–200 ms |
| Ephemeral events | Send all (typing, presence) | Aggregate, sample or drop |
| Slow clients | Small buffer, disconnect quickly | Skip ahead to latest, coalesce |
| Compression | Off (CPU and latency) | Pre-compress batches once |
| Fan-out | Direct, in-process where possible | Two-level via pub/sub or room servers |
| Delivery guarantee | In-order per room | Allow brief gaps; client refetches |

## When WebSockets aren't the best tool

- **One-to-many broadcast with few senders** (live commentary, sports scores, a streamer's 100,000
  viewers reading announcements): use **SSE**, or even **polling a cached JSON file** behind a CDN.
  If the CDN caches `/room/R/latest.json` for 1 second, your origin serves about one request per second
  per CDN location no matter how many viewers there are. Polling every 2 s scales to millions with
  almost no servers.
- **Managed services** handle connections and fan-out for you: e.g. Ably, Pusher, AWS API Gateway
  WebSockets, Cloudflare Durable Objects (one object per room is a natural fit), or self-hosted
  Centrifugo. Frameworks like Phoenix Channels (Elixir) and Socket.IO with a Redis adapter implement
  the gateway + pub/sub pattern described above.
- **Voice/video:** WebRTC with an SFU, not WebSockets.

See [polling, SSE, WebSockets and WebRTC](/posts/realtime-polling-sse-websockets) for the trade-offs
between transports.

## Summary

- A big room is a **fan-out** problem; work grows with members × message rate.
- Split **gateways** (connections) from the **fan-out layer** and deliver once per gateway, not once
  per user.
- Encode once, batch when you can, throttle ephemeral events, bound every buffer.
- Persist first with sequence numbers; treat the socket as a lossy notification channel.
- Question the requirement: the best optimisation for a 10,000-person room might be slow mode.

## Further reading

- Slack Engineering: [Real-time Messaging](https://slack.engineering/real-time-messaging/)
- Discord: [How Discord Scaled Elixir to 5,000,000 Concurrent Users](https://discord.com/blog/how-discord-scaled-elixir-to-5-000-000-concurrent-users)
- [discord/manifold](https://github.com/discord/manifold) — batching message sends per node
- RFC 6455: [The WebSocket Protocol](https://www.rfc-editor.org/rfc/rfc6455)
