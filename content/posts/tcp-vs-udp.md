+++
title = "TCP vs UDP: what they guarantee, what they cost, and when to use each"
summary = "Reliable ordered streams versus fire-and-forget datagrams: handshakes, head-of-line blocking, congestion control, and why video calls, games, DNS and HTTP/3 use UDP."
tags = ["networking", "backend", "performance"]
level = "beginner"
date = 2026-10-02
+++

Almost everything a web developer touches runs on TCP: HTTP/1.1, HTTP/2, database connections,
Redis, SSH. Yet video calls, online games, DNS lookups and HTTP/3 run on UDP. Why would anyone give up
TCP's guarantees? Because those guarantees have a price, and for some applications the price is
higher than the benefit.

## What IP gives you: almost nothing

Both protocols sit on top of IP, which delivers individual **packets** from one address to another on
a best-effort basis. Packets can be lost, duplicated, delayed or arrive out of order. IP doesn't
care. TCP and UDP are two very different answers to "what should we build on top of that?".

## UDP: datagrams, nothing more

UDP adds just two things to IP: **port numbers** (so multiple programs on a machine can share the
network) and a **checksum** (to detect corrupted packets). Each `send` is one independent message — a
**datagram**:

- No connection setup: the first packet carries data.
- No delivery guarantee: lost datagrams are simply gone.
- No ordering: datagrams may arrive in any order.
- No congestion control: UDP itself will happily flood a network.
- Message boundaries are preserved: one send = one receive (if it arrives).

## TCP: a reliable, ordered byte stream

TCP turns unreliable packets into what looks like a reliable pipe of bytes between two programs:

- **Connection setup** with a three-way handshake (`SYN`, `SYN-ACK`, `ACK`) — one round trip before
  any data flows. With TLS on top, more round trips (TLS 1.3 needs one; earlier versions two).
- **Reliability**: every byte is numbered; the receiver acknowledges what it got; missing data is
  retransmitted.
- **Ordering**: data is handed to the application in the order it was sent.
- **Flow control**: the receiver advertises how much it can buffer, so a fast sender cannot overwhelm a
  slow receiver.
- **Congestion control**: the sender starts slowly ("slow start") and backs off when packets are lost,
  sharing the network fairly.
- **A stream, not messages**: TCP doesn't preserve message boundaries. If you send "hello" and then
  "world", the receiver may read "hellowor" and then "ld". Application protocols add framing (length
  prefixes, newlines, HTTP's `Content-Length`).

```text
 client                      server
   | ---- SYN --------------> |
   | <--- SYN-ACK ----------- |    1 round trip before sending data
   | ---- ACK + request ----> |
   | <--- response ---------- |
```

## The hidden cost: head-of-line blocking

Because TCP delivers bytes strictly in order, **one lost packet stalls everything behind it** until
it's retransmitted — even data that already arrived.

```text
 sent:      [1][2][3][4][5]
 arrived:   [1]   [3][4][5]     packet 2 lost
 app sees:  [1] ........ (waits ~1 RTT for retransmit of 2) ........ [2][3][4][5]
```

For a file download that's fine — you need every byte anyway. For a **video call** it's terrible:
by the time the lost audio packet is retransmitted, that moment of the conversation has passed. You
would rather skip it and play the next one. For a **game**, the position of a player 200 ms ago is
useless once you have the current one.

This is also why HTTP/2 (many requests multiplexed over one TCP connection) can be slower than
expected on lossy mobile networks: one lost packet blocks *all* streams on the connection.

## When to use which

| Use TCP when… | Use UDP when… |
|---|---|
| Every byte must arrive (files, API calls, DB queries, payments) | Fresh data beats complete data (voice, video, game state) |
| Order matters | Each message is independent |
| You want the OS to handle reliability | You need custom reliability (retransmit only some things) |
| Connections are long-lived or setup cost is amortised | One request/one reply, setup cost dominates (DNS) |
| You need to get through restrictive firewalls | Multicast/broadcast on a local network |

Typical examples:

- **TCP:** HTTP/1.1 and HTTP/2, WebSockets, SMTP, SSH, PostgreSQL/MySQL, Redis, Kafka.
- **UDP:** DNS queries, VoIP and video conferencing (RTP inside WebRTC), online games, live streaming
  protocols, NTP, DHCP, metrics agents (StatsD), VPNs like WireGuard, and **QUIC/HTTP/3**.

## QUIC and HTTP/3: building a better TCP on UDP

Changing TCP is hard: it's implemented in operating system kernels and in every router and middlebox
that inspects traffic. So **QUIC** (standardised in 2021 as RFC 9000) implements a reliable, secure,
multiplexed transport *in user space, on top of UDP*. HTTP/3 runs on QUIC. It fixes several TCP pain
points:

- **Faster setup:** transport and TLS 1.3 handshakes are combined — one round trip, or zero for a
  resumed connection.
- **No head-of-line blocking between streams:** each stream is ordered independently, so a lost packet
  only stalls the stream it belongs to.
- **Connection migration:** connections are identified by an id, not by IP and port, so a phone
  switching from Wi-Fi to mobile data can keep its connection.
- **Always encrypted**, including most transport metadata.

You get it mostly for free: browsers, CDNs and modern proxies (Caddy, nginx, Envoy) speak HTTP/3, and
fall back to HTTP/2 over TCP when UDP is blocked.

## If you build on UDP, you inherit the hard parts

Raw UDP is rarely the right choice for application developers. If you use it, you must decide what to
do about:

- **Loss** — ignore it, retransmit selectively, or use forward error correction.
- **Ordering** — sequence numbers; drop stale packets.
- **Congestion control** — without it, your app can degrade the network for everyone, including itself.
- **Packet size** — keep datagrams under the path MTU (about 1,200–1,400 bytes is a safe payload) to
  avoid IP fragmentation.
- **Security** — encryption (DTLS) and protection against spoofed source addresses being used for
  amplification attacks.

That's why most teams use established protocols on UDP (WebRTC, QUIC, game networking libraries)
rather than inventing their own.

## Key takeaways

- **TCP**: reliable, ordered, congestion-controlled byte stream; costs a handshake and suffers
  head-of-line blocking.
- **UDP**: independent datagrams with no guarantees; minimal overhead and latency; you handle the rest.
- Choose by asking: *is late data still useful?* If yes, TCP. If no, UDP.
- **QUIC/HTTP/3** shows the modern pattern: build exactly the guarantees you need on top of UDP.

## Further reading

- [High Performance Browser Networking](https://hpbn.co/) by Ilya Grigorik — free chapters on TCP, UDP, TLS and HTTP/2
- RFC 9000: [QUIC: A UDP-Based Multiplexed and Secure Transport](https://www.rfc-editor.org/rfc/rfc9000)
- RFC 9293: [Transmission Control Protocol (TCP)](https://www.rfc-editor.org/rfc/rfc9293)
- Cloudflare Learning Center: [What is UDP?](https://www.cloudflare.com/learning/ddos/glossary/user-datagram-protocol-udp/)
