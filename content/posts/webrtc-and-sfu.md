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

The browser handles codecs (Opus for audio; VP8, VP9, H.264 or AV1 for video, depending on the
browser), echo cancellation, a **jitter buffer** (a small queue that smooths out packets arriving at
uneven times), and **congestion control** that lowers the bitrate when the network gets worse.

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

Before two peers can talk, they exchange two kinds of information:

1. **A session description (SDP).** SDP (Session Description Protocol) is a text format listing the
   media tracks a peer wants to send, the codecs it supports, and values used for security checks.
   One side creates an **offer**; the other replies with an **answer**.
2. **ICE candidates.** Each candidate is a network address (IP, port, protocol) where the peer might
   be reachable. Peers send them as soon as they find them ("trickle ICE").

How these messages travel is up to you, usually over a WebSocket to your own server (see
[polling, SSE, WebSockets and WebRTC](/posts/realtime-polling-sse-websockets)).

```text
 Alice (browser)            signaling server (WebSocket)            Bob (browser)
   | createOffer()                   |                                   |
   | --- offer (SDP) --------------> | --- offer (SDP) ----------------> |
   |                                 |                   createAnswer()  |
   | <------------ answer (SDP) ---- | <-------------- answer (SDP) ---- |
   | --- ICE candidates -----------> | -------------------------------> |
   | <------------------------------ | <------------- ICE candidates --- |
   |                                                                     |
   | <===== media flows peer to peer (or via TURN), not via signaling ==> |
```

A trimmed SDP offer looks like this:

```text
m=audio 9 UDP/TLS/RTP/SAVPF 111
a=ice-ufrag:EsAw
a=ice-pwd:bP+XJMM09aR8AiX1jdukzR6Y
a=fingerprint:sha-256 7B:8B:F0:...:A2
a=rtpmap:111 opus/48000/2
```

The caller's side in the browser, without error handling:

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
relays messages. It must also handle reconnects, and both peers sending an offer at once ("glare");
MDN documents a "perfect negotiation" pattern for that.

## NAT traversal: ICE, STUN and TURN

Most devices sit behind a **NAT** (Network Address Translation): your router gives your laptop a
private address like `192.168.1.20` and rewrites outgoing packets to use its own public address and
some port. Nobody outside can send to `192.168.1.20` directly.

**ICE** (Interactive Connectivity Establishment) finds a path anyway. Each peer collects candidates:

| Candidate type | What it is | Found by |
|---|---|---|
| `host` | The device's own address, often private | Reading local interfaces |
| `srflx` (server reflexive) | The public address and port the NAT uses for you | Asking a STUN server |
| `relay` | An address on a TURN server that forwards your packets | Allocating it on a TURN server |

- **STUN** (Session Traversal Utilities for NAT): the browser asks a STUN server "which address do
  you see me as?". STUN is cheap: tiny requests, no media.
- **Connectivity checks:** both peers send test packets to each other's candidates. An outgoing packet
  opens a short-lived mapping in each NAT, so replies can come back in ("UDP hole punching"). ICE
  picks the best working pair and prefers direct paths over relays.
- **TURN** (Traversal Using Relays around NAT) is the fallback. Some NATs (often called symmetric
  NATs) use a different public port for every destination, so the STUN address is useless to the
  peer. Some firewalls block UDP completely. Then media goes through a TURN server, which relays it.
  TURN over TLS on port 443 gets through many strict corporate networks.

### The cost of TURN

TURN carries **every media packet, in both directions, for the whole call**. A one-to-one call where
each person sends 1.5 Mbit/s, fully relayed:

```text
TURN egress = 2 streams x 1.5 Mbit/s = 3 Mbit/s
one hour    = 3 Mbit/s x 3,600 s = 10,800 Mbit = about 1.35 GB of outbound traffic
```

With cloud egress pricing, that adds up. Most calls usually connect directly, but you cannot predict
which will not, so a real product needs TURN. Measure the share of calls that use a `relay`
candidate (`getStats()` reports the selected candidate pair) and plan capacity from that.

> [!WARNING]
> Never ship long-lived TURN passwords in frontend code: anyone can copy them and relay traffic
> through your server for free. Issue **short-lived credentials** from your API (coturn, a widely used
> open-source TURN server, supports a shared-secret scheme for this). Also block relaying to your
> private network ranges, or attackers can use TURN to reach internal services.

## Encryption is always on

WebRTC media is always encrypted; there is no switch to turn it off. **DTLS** (TLS adapted for UDP)
runs a handshake between the peers, and each peer proves it holds the certificate whose fingerprint
was in its SDP. The resulting keys protect media with **SRTP**, the secure version of RTP (Real-time
Transport Protocol, the packet format for audio and video). Data channels run inside DTLS directly.

Two consequences:

- Security depends on your **signaling**. Whoever can change the SDP in transit can swap the
  fingerprint. Use `wss://` and authenticate users.
- Encryption is **hop by hop**. An SFU is a WebRTC endpoint, so it can decrypt the media. For
  **end-to-end encryption**, the app encrypts each frame again before sending, for example with the
  browser's encoded-transform APIs (also called "insertable streams") or SFrame. Then the server
  cannot record or mix without the keys.

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

Take **N** participants, each sending video at bitrate **b**, and ignore audio. The numbers use
b = 1 Mbit/s as an example, not a recommendation:

| | Mesh, per client up / down | SFU, per client up / down | SFU, server sends | MCU, per client up / down |
|---|---|---|---|---|
| Formula | (N-1)b / (N-1)b | b / (N-1)b | N(N-1)b | b / b |
| N = 4 | 3 / 3 Mbit/s | 1 / 3 Mbit/s | 12 Mbit/s | 1 / 1 Mbit/s |
| N = 10 | 9 / 9 Mbit/s | 1 / 9 Mbit/s | 90 Mbit/s | 1 / 1 Mbit/s |
| N = 25 | 24 / 24 Mbit/s | 1 / 24 Mbit/s | 600 Mbit/s | 1 / 1 Mbit/s |

Mesh fails on **upload** and CPU, because the browser usually encodes the video separately for each
connection. It is fine for two people and tolerable for three or four. The SFU is the usual choice;
its weak spots are client **download** and server outbound traffic, which grow with N. The MCU is
easy on clients, but the server decodes and re-encodes every stream, which costs a lot of CPU and
adds delay. Today it is mostly used to connect phones and older systems that expect one mixed stream.

## Simulcast and SVC: the right quality for each viewer

Nobody watches 24 full HD videos at once. A good SFU sends each viewer only what they can see and
their network can carry, without decoding anything:

- **Simulcast:** the sender encodes the camera at two or three sizes and sends all of them. For each
  viewer the SFU forwards one: large for the active speaker, small for thumbnails, none for people
  scrolled out of view. The small copies add little upload.
- **SVC** (Scalable Video Coding): the sender produces one stream built in **layers**. The base layer
  works alone; extra layers add frame rate or resolution. The SFU drops layers per viewer. VP9 and AV1
  support this; browser support varies, so test first.

When the SFU switches a viewer to another layer, the decoder needs a **keyframe** (a full picture that
does not depend on earlier frames), so the SFU asks the sender for one. SFUs can also cap how many
videos they forward; Jitsi calls this "Last N": video only for the N most recent speakers.

## Data channels

An `RTCDataChannel` sends messages over **SCTP** (Stream Control Transmission Protocol), inside DTLS,
over the same UDP path as the media. Unlike WebSockets, you choose the delivery rules per channel:

```js
// Game state: newest data wins, so never retransmit and never wait for order.
const state = pc.createDataChannel("state", { ordered: false, maxRetransmits: 0 });
// File transfer: reliable and ordered, like TCP (the default).
const files = pc.createDataChannel("files");
```

Use them for low-latency peer-to-peer data: game input, cursor positions, file transfer (in small
chunks). For chat inside a call, your WebSocket is simpler and already stores messages.

## Open-source SFUs

You rarely write an SFU yourself. Well-known open-source options:

| Project | What it is |
|---|---|
| mediasoup | A library: a Node.js (or Rust) API with C++ media workers. You build signaling and rooms. |
| Janus | A general-purpose WebRTC server in C with plugins; the VideoRoom plugin is an SFU. |
| LiveKit | A complete SFU in Go, built on the Pion library, with signaling and client SDKs included. |
| Jitsi Videobridge | The SFU behind Jitsi Meet, running on the JVM. |

A library gives full control; a complete server gets you working calls faster. If calls are not
your core product, a managed video API is also reasonable.

## Scaling SFUs and recording

An SFU does not encode video, so its limits are usually **outbound bandwidth**, **packets per second**
and encryption CPU. Load-test with real or headless browsers; do not trust a single "users per
server" number.

- **Room placement.** Usually a whole room lives on one SFU. A coordination service (often backed by
  Redis or a database) records which SFU hosts each room, and signaling tells clients where to
  connect. Moving a live call forces every client to reconnect, so for deploys you **drain** servers:
  no new rooms, then wait for calls (which can last hours) to end. See
  [zero-downtime deployments](/posts/deployment-strategies-and-zero-downtime-migrations).
- **Networking.** Clients send UDP straight to the SFU, so each SFU needs a public IP in its ICE
  candidates and open UDP ports. An HTTP load balancer cannot sit in the media path.
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
[big chat rooms](/posts/scaling-websockets-chat). The cost is that room state (who publishes which
track) must be shared across servers. Jitsi has described cascaded bridges under the name "Octo".

**Recording** has two common designs:

1. **Per-track.** The SFU forwards each participant's packets to a recorder that writes them to files
   without re-encoding. Cheap, but merging tracks into one video is a later
   [background job](/posts/background-jobs-and-cron), and syncing tracks with gaps is tricky.
2. **Composite.** A headless browser joins as a hidden participant, renders the layout, and its screen
   and audio are encoded into one file or stream. Jitsi's Jibri and LiveKit Egress work this way. It
   is CPU-heavy, like an MCU, so give recorders their own machines.

Either way, tell participants they are being recorded; in many places the law requires it.

## When not to use WebRTC

- **Broadcasting to a large audience** that accepts a few seconds of delay: HLS or DASH (video cut
  into small files served over HTTP) through a CDN is far cheaper. Use WebRTC when viewers must
  interact in real time.
- **Chat, notifications, server-to-client updates:** WebSockets or SSE are simpler.

## In practice: a checklist

- [ ] Two people: peer to peer with STUN, TURN as fallback. Three or more: an SFU.
- [ ] TURN on UDP 3478 and TLS 443, with short-lived credentials.
- [ ] Signaling over `wss://` with real authentication and room authorisation.
- [ ] Simulcast (or SVC) for video in group calls; only visible videos forwarded.
- [ ] SFUs close to users, rooms placed by load, draining for deploys.
- [ ] Client stats from `getStats()` collected: packet loss, round-trip time, jitter, relayed or not
      (see [observability](/posts/observability-logs-metrics-traces)).

## Common mistakes

- **No TURN server.** Everything works in the office and fails for some customers.
- **Mesh for big calls.** Fine with four people on fibre; broken with eight on Wi-Fi.
- **Thinking DTLS-SRTP means end-to-end.** With an SFU, the server can see the media.
- **SFU behind an HTTP load balancer**, or without a public IP or open UDP ports.
- **Naive signaling.** Reconnects, glare and network changes (Wi-Fi to mobile needs an ICE restart)
  need careful state handling.

## Further reading

- [WebRTC for the Curious](https://webrtcforthecurious.com/): a free book on how the protocols work
- MDN: [WebRTC API](https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API)
- RFC 8445: [Interactive Connectivity Establishment (ICE)](https://www.rfc-editor.org/rfc/rfc8445)
- RFC 8656: [Traversal Using Relays around NAT (TURN)](https://www.rfc-editor.org/rfc/rfc8656)
- [High Performance Browser Networking](https://hpbn.co/) by Ilya Grigorik: includes a chapter on WebRTC
