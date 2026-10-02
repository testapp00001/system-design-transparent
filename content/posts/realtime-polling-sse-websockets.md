+++
title = "Real-time on the web: polling, long polling, SSE, WebSockets and WebRTC"
summary = "Five ways to push fresh data to users, how each works on the wire, what each costs on the server, and a simple guide to choosing the right one."
tags = ["realtime", "networking", "backend"]
level = "intermediate"
date = 2026-10-02
+++

HTTP was designed for request and response: the browser asks, the server answers. But chat messages,
notifications, live scores, dashboards and collaborative editors need the **server** to tell the
**client** that something changed. There are five common ways to do that. None is "the best"; each
fits different requirements.

## 1. Short polling

The client asks on a timer: "anything new?"

```js
setInterval(async () => {
  const res = await fetch("/api/notifications?since=" + lastId);
  render(await res.json());
}, 5000);
```

- **Latency:** up to the interval (here, 5 s).
- **Cost:** a full HTTP request every interval per client, even when nothing changed. 10,000 clients
  polling every 5 s is 2,000 requests/second of mostly empty answers.
- **Strengths:** trivially simple, works through every proxy and firewall, stateless servers, easy to
  cache, easy to scale with a normal load balancer.

Polling is underrated. For dashboards refreshing every 30 seconds, or "your export is ready" messages,
it is often the right answer.

## 2. Long polling

The client asks, and the server **holds the request open** until there is something to say (or a
timeout, e.g. 30 s). Then the client immediately asks again.

```text
client: GET /updates?since=41 ...................(server waits)........ 200 [msg 42]
client: GET /updates?since=42 ......(waits)...... 200 [msg 43, 44]
client: GET /updates?since=44 .............................(30 s)...... 204 (nothing)
```

- **Latency:** near real time.
- **Cost:** one pending request per client; a new HTTP request after every message. Bursty traffic
  means a lot of request overhead.
- **Strengths:** works everywhere plain HTTP works. It was the standard technique before WebSockets
  and is still a common fallback.

## 3. Server-Sent Events (SSE)

The client opens one long-lived HTTP response, and the server writes events into it as they happen.
It's a W3C/WHATWG standard with a built-in browser API:

```js
const events = new EventSource("/stream");
events.onmessage = (e) => render(JSON.parse(e.data));
```

On the wire it's just text over a normal HTTP response:

```text
HTTP/1.1 200 OK
Content-Type: text/event-stream

id: 42
data: {"type":"message","text":"hello"}

id: 43
data: {"type":"message","text":"world"}
```

- **Direction:** server → client only. The client sends data with ordinary HTTP requests.
- **Built-in reconnect:** the browser reconnects automatically and sends `Last-Event-ID`, so the
  server can resume from where the client left off.
- **Strengths:** plain HTTP (works with HTTP/2 multiplexing, existing auth cookies, most proxies),
  very simple server code, perfect for feeds, notifications, live dashboards, progress updates and
  streaming LLM responses.
- **Watch out:** over HTTP/1.1 browsers limit connections per domain (about 6), so many open tabs can
  exhaust them; HTTP/2 removes this problem. Some proxies buffer responses — disable buffering for the
  stream endpoint.

## 4. WebSockets

A WebSocket starts as an HTTP request with an `Upgrade: websocket` header. After the handshake the
TCP connection becomes a **full-duplex message channel**: both sides can send messages (frames) at any
time with very little overhead per message.

```js
const ws = new WebSocket("wss://chat.example.com/ws");
ws.onmessage = (e) => render(JSON.parse(e.data));
ws.send(JSON.stringify({ type: "message", room: 7, text: "hi" }));
```

- **Latency:** lowest of the HTTP-based options; no per-message HTTP headers.
- **Direction:** both ways, which matters when clients send frequently (chat, games, collaborative
  editing, trading).
- **Costs:**
  - Every connected user holds an open connection (memory, a file descriptor) on some server for
    hours.
  - Connections are **stateful**: you must know which server holds which user to deliver a message to
    them — see [scaling WebSockets](/posts/scaling-websockets-chat).
  - You implement yourself what HTTP gave you: reconnection, resuming missed messages, heartbeats
    (to detect dead connections), authentication on connect, backpressure.
  - Deploys disconnect everyone on the restarted server, and they all reconnect at once.

## 5. WebRTC

Peer-to-peer connections between browsers, mostly over **UDP**, designed for audio and video calls,
screen sharing and low-latency data channels. It needs signaling servers to set up calls and STUN/TURN
servers to traverse NATs, and for group calls usually an SFU (selective forwarding unit) server.
Use it for media; don't use it to deliver chat messages.

## Comparison

| | Polling | Long polling | SSE | WebSockets | WebRTC |
|---|---|---|---|---|---|
| Direction | client pulls | server → client | server → client | both | both (peer-to-peer) |
| Latency | interval | low | low | lowest | lowest (UDP) |
| Server state per client | none | pending request | open stream | open socket | media sessions |
| Works through proxies | always | almost always | usually | usually | needs TURN |
| Auto-reconnect/resume | n/a | manual | built in | manual | manual |
| Complexity | very low | low | low | medium–high | high |

## How to choose

```text
Updates every ≥10 s and staleness is fine?            -> polling (cache it!)
Server pushes, client rarely sends?                   -> SSE
  (notifications, feeds, live scores, progress, LLM token streaming)
Both sides send often, latency matters?               -> WebSockets
  (chat with typing indicators, multiplayer, collaborative editing)
Audio/video or peer-to-peer?                          -> WebRTC
Must work behind hostile corporate proxies?           -> long polling fallback
```

Two pieces of advice that hold regardless of transport:

1. **The transport is not the source of truth.** Store messages in a database or log first, with
   increasing ids. The real-time channel only *notifies*. When a client reconnects, it asks "what
   happened after id 1042?" and catches up. This makes lost connections harmless.
2. **Start simple.** Many products that "need WebSockets" work perfectly with SSE plus normal POST
   requests, and SSE scales with ordinary HTTP infrastructure.

## Further reading

- MDN: [Server-sent events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events)
- MDN: [The WebSocket API](https://developer.mozilla.org/en-US/docs/Web/API/WebSockets_API)
- RFC 6455: [The WebSocket Protocol](https://www.rfc-editor.org/rfc/rfc6455)
- [High Performance Browser Networking](https://hpbn.co/) by Ilya Grigorik — free online book, chapters on XHR, SSE and WebSocket
