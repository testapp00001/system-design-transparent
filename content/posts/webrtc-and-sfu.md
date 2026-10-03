+++
title = "WebRTC explained: signaling, STUN/TURN and SFUs for group calls"
summary = "How browsers set up audio, video and data connections: signaling with SDP and ICE, NAT traversal with STUN and TURN, always-on encryption, and why group calls need an SFU, with bandwidth math, simulcast and scaling across servers."
tags = ["realtime","networking"]
level = "advanced"
date = 2026-10-02
+++

Your product manager asks for video calls. You follow a tutorial, open two browser tabs, and it works
on the first try. Then real users arrive. A customer behind a corporate firewall sees a black screen.
A team of eight joins one call and every laptop fan spins at full speed. WebRTC is not broken. You
have met the three servers that tutorials skip: **signaling**, **TURN** and an **SFU**.

## What WebRTC gives you (and what it doesn't)

**WebRTC** (Web Real-Time Communication) is a set of browser APIs and network protocols, standardised
by the W3C and the IETF. The main APIs:

- `getUserMedia()`: access the camera and microphone.
- `RTCPeerConnection`: send and receive audio and video with another endpoint.
- `RTCDataChannel`: send arbitrary messages over the same connection.

The browser handles codecs, echo cancellation, a **jitter buffer** (a small queue that smooths out
packets arriving at uneven times), and **congestion control** (lowering the bitrate when the network
gets worse). The standards require every browser to support the Opus and G.711 (an old telephone
codec) audio codecs, and the VP8 and H.264 video codecs. Support for VP9 and AV1 differs by browser
and version.

Media travels mostly over **UDP**, because in a call a late packet is useless, and TCP would stall
everything behind one lost packet (see [TCP vs UDP](/posts/tcp-vs-udp)).

```text
   media (audio, video)            data channels
          RTP                          SCTP
          SRTP  <---- keys from ----   DTLS
            \                          /
             +---------- ICE ---------+      finds a network path that works
                          |
            UDP  (TCP or a TURN relay only as a fallback)
```

What WebRTC does **not** give you: a way for two browsers to find each other (**signaling**),
servers for NAT traversal (**STUN** and **TURN**), and anything about rooms, users, permissions,
recording or group calls. You build or run those.

## Signaling: the part you build

Before two peers can talk, they exchange two kinds of messages:

1. **A session description (SDP).** SDP (Session Description Protocol) is a text format listing the
   media tracks a peer wants to send, the codecs it supports, and values used for security checks.
   One side creates an **offer**; the other replies with an **answer**.
2. **ICE candidates.** Each candidate is a network address (IP, port, protocol) where the peer might
   be reachable. Peers send them as soon as they find them ("trickle ICE").

How these messages travel is up to you, usually over a WebSocket to your own server (see
[polling, SSE, WebSockets and WebRTC](/posts/realtime-polling-sse-websockets)).

```text
 Alice (browser)       signaling server (WebSocket)                Bob (browser)
   | createOffer()                   |                                   |
   | --- offer (SDP) --------------> | --- offer (SDP) ----------------> |
   |                                 |                   createAnswer()  |
   | <------------ answer (SDP) ---- | <-------------- answer (SDP) ---- |
   | --- ICE candidates -----------> | --------------------------------> |
   | <------------------------------ | <------------- ICE candidates --- |
   |                                                                     |
   | <===== media flows peer to peer (or via TURN), not via signaling => |
```

A trimmed SDP offer looks like this:

```text
m=audio 9 UDP/TLS/RTP/SAVPF 111
a=ice-ufrag:EsAw
a=ice-pwd:bP+XJMM09aR8AiX1jdukzR6Y
a=fingerprint:sha-256 7B:8B:F0:...:A2
a=rtpmap:111 opus/48000/2
```

The caller's side, without error handling (`turnCreds` holds short-lived TURN credentials that your
own API returned; see the warning below):

```js
const pc = new RTCPeerConnection({
  iceServers: [
    { urls: "stun:stun.example.com:3478" },
    { urls: ["turn:turn.example.com:3478", "turns:turn.example.com:443?transport=tcp"],
      username: turnCreds.username, credential: turnCreds.password },
  ],
});
const ws = new WebSocket("wss://signal.example.com/rooms/42");
await new Promise((resolve) => (ws.onopen = resolve));

ws.onmessage = async ({ data }) => {
  const msg = JSON.parse(data);
  if (msg.type === "answer") await pc.setRemoteDescription(msg.sdp);
  if (msg.type === "candidate") await pc.addIceCandidate(msg.candidate);
};
pc.onicecandidate = (e) => {
  if (e.candidate) ws.send(JSON.stringify({ type: "candidate", candidate: e.candidate }));
};

const stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: true });
stream.getTracks().forEach((track) => pc.addTrack(track, stream));
await pc.setLocalDescription(await pc.createOffer());
ws.send(JSON.stringify({ type: "offer", sdp: pc.localDescription }));
```

The signaling server is an ordinary backend service: it authenticates users, checks room access and
relays messages. It must also handle reconnects and "glare" (both peers sending an offer at once);
MDN documents a "perfect negotiation" pattern for that.

## NAT traversal: ICE, STUN and TURN

Most devices sit behind a **NAT** (Network Address Translation): your router gives your laptop a
private address like `192.168.1.20` and rewrites outgoing packets to use its own public address and
some port. Nobody outside can send to `192.168.1.20` directly.

**ICE** (Interactive Connectivity Establishment) finds a path anyway. Each peer collects candidates:

| Candidate type | What it is |
|---|---|
| `host` | The device's own address, often private |
| `srflx` (server reflexive) | Your public address and port on the NAT, as seen by a STUN server |
| `relay` | An address on a TURN server that forwards your packets |

- **STUN** (Session Traversal Utilities for NAT): the browser asks a STUN server "which address do
  you see me as?". STUN is cheap: tiny requests, no media.
- **Connectivity checks:** both peers send test packets to each other's candidates. Each outgoing
  packet opens a short-lived mapping in the sender's own NAT, so packets from the other peer can come
  back in ("UDP hole punching"). ICE picks the best working pair and prefers direct paths over relays.
- **TURN** (Traversal Using Relays around NAT) is the fallback. Some NATs (often called symmetric
  NATs) use a different public port for each destination, so the address that STUN reported is
  often not the one the other peer would need, and a direct path can fail. Some firewalls block UDP
  completely. Then media goes through a TURN server, which relays it. TURN over TLS on port 443 gets
  through many strict corporate networks, because it uses the HTTPS port and is wrapped in TLS.

### The cost of TURN

TURN carries **every media packet, in both directions, for the whole call**. Take a relayed
one-to-one call through one TURN server, where each person sends 1.5 Mbit/s. The server forwards
each person's stream to the other, so it sends 2 x 1.5 = 3 Mbit/s. One hour is 3 x 3,600 = 10,800
Mbit, about 1.35 GB of outbound traffic. With cloud egress pricing, that adds up. Usually most calls
connect directly, but you cannot predict which ones will not, so a real product needs TURN. Measure
the share of relayed calls (`getStats()` reports the selected candidate pair and its candidate
types) and plan capacity from that.

> [!WARNING]
> Never ship long-lived TURN passwords in frontend code: anyone can copy them and relay traffic
> through your server for free. Issue **short-lived credentials** from your API (coturn, a widely used
> open-source TURN server, supports a shared-secret scheme for this with its `use-auth-secret` and
> `static-auth-secret` options). Also block relaying to your private network ranges (in coturn, with
> `denied-peer-ip`), or attackers can use TURN to reach internal services.

## Encryption is always on

WebRTC media is always encrypted; there is no switch to turn it off. **DTLS** (TLS adapted for UDP)
runs a handshake between the peers, and each peer proves it holds the certificate whose fingerprint
was in its SDP. The resulting keys protect media with **SRTP**, the secure version of RTP (Real-time
Transport Protocol, the packet format for audio and video). Data channels run inside DTLS directly.
Two consequences:

- Security depends on your **signaling**. Whoever can change the SDP in transit can swap the
  fingerprint. Use `wss://` and authenticate users.
- Encryption runs **between WebRTC endpoints**. In a two-person call that is end to end; a TURN
  server only forwards encrypted packets and cannot read them. But an SFU is itself a WebRTC
  endpoint, so it decrypts the media it receives and encrypts it again for each receiver. For
  **end-to-end encryption**, the app encrypts each frame again before sending. Browsers expose the
  encoded frames through the WebRTC Encoded Transform API (an earlier Chrome version was called
  "insertable streams"); the IETF SFrame format is one standard way to encrypt them. The keys are
  shared between participants, never with the server, so the server cannot record or mix the media.

## Group calls: mesh, SFU and MCU

With more than two people, there are three topologies:

- **Mesh:** every participant connects directly to every other one. No media server.
- **SFU** (Selective Forwarding Unit): everyone sends one stream to a server, which **forwards**
  packets to the others without decoding them.
- **MCU** (Multipoint Control Unit): the server **decodes** all streams, mixes them into one picture
  and one audio track, encodes the result and sends it back.

```text
      mesh (no server)                 SFU or MCU
   A ------------ B                A              B
   | \          / |                  \          /
   |    \    /    |                   [ server ]     SFU: forwards packets
   |    /    \    |                  /          \    MCU: decodes, mixes, re-encodes
   | /          \ |                C              D
   C ------------ D
```

For **N** participants each sending video at bitrate **b** (audio ignored; b = 1 Mbit/s is only an
example):

| | Mesh, per client up / down | SFU, per client up / down | SFU, server sends | MCU, per client up / down |
|---|---|---|---|---|
| Formula | (N-1)b / (N-1)b | b / (N-1)b | N(N-1)b | b / b |
| N = 4 | 3 / 3 Mbit/s | 1 / 3 Mbit/s | 12 Mbit/s | 1 / 1 Mbit/s |
| N = 10 | 9 / 9 Mbit/s | 1 / 9 Mbit/s | 90 Mbit/s | 1 / 1 Mbit/s |
| N = 25 | 24 / 24 Mbit/s | 1 / 24 Mbit/s | 600 Mbit/s | 1 / 1 Mbit/s |

Mesh fails on **upload** and CPU, because the browser usually encodes the video once per
connection. It is fine for two people and tolerable for three or four. The SFU is the usual choice;
its weak spots, client **download** and server outbound traffic, grow with N. The MCU is easy on
clients, but decoding and re-encoding every stream costs a lot of server CPU and adds delay. Today
mixing is typically used where a client can only handle one stream, for example phone dial-in or
older conference-room systems.

## Simulcast and SVC: the right quality for each viewer

Nobody watches 24 full HD videos at once. A good SFU sends each viewer only what they can see and
their network can carry, without decoding anything:

- **Simulcast:** the sender encodes the camera at two or three sizes and sends all of them. For each
  viewer the SFU forwards one: large for the active speaker, small for thumbnails, none for people
  scrolled out of view. The small copies cost only a modest amount of extra upload.
- **SVC** (Scalable Video Coding): the sender produces one stream built in **layers**. The base layer
  works alone; extra layers add frame rate or resolution. The SFU drops layers per viewer. VP9 and AV1
  can add both kinds of layers, VP8 only frame-rate layers. Browser support varies, so test first.

When the SFU switches a viewer to another layer, the decoder needs a **keyframe** (a full picture that
does not depend on earlier frames), so the SFU asks the sender for one. SFUs can also cap how many
videos they forward; Jitsi calls this "Last N": video only for the N most recently active speakers.

## Data channels

An `RTCDataChannel` sends messages over **SCTP** (Stream Control Transmission Protocol), inside DTLS,
over the same UDP path as the media. Unlike WebSockets, you choose the delivery rules per channel:

```js
// Game state: newest data wins, so no retransmits and no ordering.
const state = pc.createDataChannel("state", { ordered: false, maxRetransmits: 0 });
// File transfer: reliable and ordered (the default).
const files = pc.createDataChannel("files");
```

Use them for low-latency peer-to-peer data: game input, cursor positions, file transfer (in small
chunks).

## Open-source SFUs

You rarely write an SFU yourself. Well-known open-source options:

| Project | What it is |
|---|---|
| mediasoup | A library: a Node.js (or Rust) API with C++ media workers. You build signaling and rooms. |
| Janus | A general-purpose WebRTC server in C with plugins; the VideoRoom plugin is an SFU. |
| LiveKit | A complete SFU server in Go (built on the Pion library), with signaling and client SDKs. |
| Jitsi Videobridge | The SFU behind Jitsi Meet, running on the JVM. |

A library gives full control; a complete server gets you working calls faster. If calls are not
your core product, a managed video API is also reasonable.

## Scaling SFUs and recording

An SFU does not encode video, so its limits are usually **outbound bandwidth**, **packets per second**
and encryption CPU. Load-test with real or headless browsers rather than trusting a "users per
server" number.

- **Room placement.** Usually a whole room lives on one SFU. A coordination service records which
  SFU hosts each room, and signaling tells clients where to connect. Moving a live call forces every
  client to reconnect, so for deploys you **drain** servers: no new rooms, then wait for calls
  (which can last hours) to end. See
  [zero-downtime deployments](/posts/deployment-strategies-and-zero-downtime-migrations).
- **Networking.** Clients send UDP straight to the SFU, so each SFU needs a public IP in its ICE
  candidates and open UDP ports. On a cloud VM that only sees a private IP, you configure the public
  IP in the SFU's settings. An HTTP load balancer cannot sit in the media path.
- **Cascading.** When a room outgrows one server or spans continents, SFUs forward streams to each
  other:

```text
        Europe                                           US East
   [alice]  [bob]                                    [carol]  [dan]
       \     /                                           \     /
     [ SFU eu-1 ] <==== one copy of each stream ====> [ SFU us-1 ]
```

Each person connects to a nearby SFU, so lost packets are resent over a short distance, and each
stream crosses the ocean once, not once per viewer: the same two-level fan-out used for
[big chat rooms](/posts/scaling-websockets-chat). The cost: room state (who publishes which track)
is shared across servers. Jitsi has described cascaded bridges under the name "Octo".

**Recording** has two common designs:

1. **Per-track.** The SFU forwards each participant's packets to a recorder that saves them without
   re-encoding. Cheap, but merging tracks into one video is a later
   [background job](/posts/background-jobs-and-cron), and syncing tracks with gaps is tricky.
2. **Composite.** A browser with no real screen (headless, or drawing to a virtual display) joins as
   a hidden participant and renders the layout. Its picture and sound are encoded into one file or
   stream. Jitsi's Jibri and LiveKit's room-composite Egress work this way. It is CPU-heavy, like an
   MCU, so give recorders their own machines.

Either way, tell participants they are being recorded; in many places the law requires it.

## When not to use WebRTC

- **Broadcasting to a large audience** that accepts a few seconds of delay: HLS or DASH (video cut
  into small files served over HTTP) through a CDN is far cheaper. Use WebRTC when viewers must
  interact in real time.
- **Chat, notifications, server-to-client updates:** WebSockets or SSE are simpler.

## In practice: a checklist

- [ ] Two people: peer to peer, TURN as fallback. Three or more: usually an SFU.
- [ ] TURN on UDP 3478 and TLS 443, with short-lived credentials. (If your web server already uses
      port 443, TURN usually needs its own IP address.)
- [ ] Signaling over `wss://`, with authentication and room authorisation.
- [ ] Simulcast (or SVC) in group calls; only visible videos forwarded.
- [ ] SFUs close to users, rooms placed by load, draining for deploys.
- [ ] Client stats from `getStats()` collected: packet loss, round-trip time, jitter, relayed or not
      (see [observability](/posts/observability-logs-metrics-traces)).

## Common mistakes

- **No TURN server.** Everything works in the office and fails for some customers.
- **Thinking DTLS-SRTP means end-to-end.** With an SFU, the server can see the media.
- **SFU behind an HTTP load balancer**, or without a public IP and open UDP ports.
- **Naive signaling.** Reconnects, glare and network changes (Wi-Fi to mobile needs an ICE restart)
  need careful state handling.

## Further reading

- [WebRTC for the Curious](https://webrtcforthecurious.com/): a free book on how the protocols work
- MDN: [WebRTC API](https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API)
- RFC 8445: [Interactive Connectivity Establishment (ICE)](https://www.rfc-editor.org/rfc/rfc8445)
- [High Performance Browser Networking](https://hpbn.co/) by Ilya Grigorik: includes a chapter on WebRTC
