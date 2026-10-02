+++
title = "HTTP/1.1, HTTP/2 and HTTP/3: what changed and why it matters"
summary = "Why HTTP/2 moved to one multiplexed connection, why HTTP/3 moved to QUIC over UDP, and what each change means for load balancing, gRPC, proxies and enabling HTTP/3 in production."
tags = ["networking", "performance"]
level = "intermediate"
date = 2026-10-02
+++

You open your browser's developer tools, go to the Network tab and turn on the **Protocol** column.
Some requests say `http/1.1`, some `h2`, some `h3`. Your application server only speaks HTTP/1.1, so
where did `h3` come from? The same week, a colleague wants to add gRPC, and someone says "our load
balancer doesn't handle HTTP/2 properly". To answer these questions you need to know what each HTTP
version changed, and which problem each change solved.

The good news: the *meaning* of HTTP did not change. Methods, status codes, headers and caching rules
are the same in all three versions (defined once, in RFC 9110 and RFC 9111). What changed is how
requests and responses travel over the network.

| Version | First standardised | Runs on | Message format | Big idea |
|---|---|---|---|---|
| HTTP/1.1 | 1997 (RFC 2068; today RFC 9112) | TCP | Text | Reuse connections (keep-alive) |
| HTTP/2 | 2015 (RFC 7540; today RFC 9113) | TCP, with TLS in practice | Binary frames | Many requests on one connection |
| HTTP/3 | 2022 (RFC 9114) | QUIC, which runs on UDP | Binary frames | Move streams into the transport |

## HTTP/1.1: one request at a time per connection

### Keep-alive

Early HTTP opened a new TCP connection for every request. Each one costs a TCP handshake, usually a
TLS handshake, and a "slow start" while TCP learns how fast it can send (see
[TCP vs UDP](/posts/tcp-vs-udp)). HTTP/1.1 made **persistent connections** (**keep-alive**) the
default: after a response, the connection stays open for the next request, until either side closes
it or an idle timeout expires.

### Head-of-line blocking

Keep-alive saves handshakes, but a connection still carries **one request and one response at a
time**. If the response to request A is slow, request B waits behind it, even if B is tiny. This is
**head-of-line blocking**: the first item in the line blocks everything behind it.

HTTP/1.1 also defined **pipelining** (sending several requests without waiting), but responses must
still come back in order, and many proxies handled it badly. In practice browsers do not use it.

### Six connections per host

Browsers work around the problem by opening several connections to the same host in parallel,
usually up to **about six**. A page with 80 small files on one host therefore downloads at most six
at a time.

```text
HTTP/1.1: each connection carries one request/response at a time

conn 1: [req a .... resp a][req g .. resp g][req m ...
conn 2: [req b .. resp b][req h .... resp h][req n ...
  ...
conn 6: [req f ....... slow resp f .......][req l ...
         later requests queue until a connection is free
```

Every extra connection has its own handshake, its own slow start and its own memory on the server.

### The old workarounds

Web developers invented tricks to send fewer, larger requests:

| Workaround | What it does | Its cost |
|---|---|---|
| Domain sharding | Serve assets from `static1.example.com`, `static2.example.com`... to get six connections *per hostname* | More DNS lookups, TCP and TLS handshakes |
| CSS sprites | Combine many icons into one image; show parts of it with CSS | Changing one icon invalidates the whole image in caches |
| Bundling (concatenation) | Merge many JavaScript or CSS files into one | Same cache problem; ships code the page may not need |
| Inlining | Put small images or CSS directly into the HTML | Cannot be cached separately |

## HTTP/2: many streams over one TCP connection

HTTP/2 grew out of SPDY, an experimental protocol from Google. It keeps HTTP's meaning but changes
the wire format completely.

### Binary framing and streams

HTTP/2 splits every message into **frames**. Each frame has a 9-byte header: length, type
(`HEADERS`, `DATA`, `SETTINGS`, `GOAWAY` and others), flags, and a **stream ID**. A **stream** is one
request and its response; streams started by the client use odd numbers (1, 3, 5...). Because every
frame names its stream, frames from many streams can be **interleaved** on one connection. This is
**multiplexing**:

```text
HTTP/2: one TCP connection, frames from many streams interleaved

client -> server:  [HEADERS 1][HEADERS 3][HEADERS 5]
server -> client:  [HEADERS 3][DATA 3][HEADERS 1][DATA 1][DATA 3][HEADERS 5][DATA 1][DATA 5]

A slow response on stream 1 no longer blocks streams 3 and 5.
```

So browsers usually open **one connection per origin** instead of six. Each side announces how many
streams the other side may have open at the same time (`SETTINGS_MAX_CONCURRENT_STREAMS`); RFC 9113
recommends a value of at least 100. **Flow control**, per stream and for the whole connection, stops
one large download from filling all the buffers.

### HPACK header compression

HTTP/1.1 sends headers as plain text with every request. Cookies, `User-Agent` and authorization
headers repeat again and again. HTTP/2 compresses them with **HPACK** (RFC 7541): common headers
like `:method: GET` come from a **static table** and are sent as a small number; both sides keep a
**dynamic table** of headers already sent on this connection, so a repeated cookie becomes a short
reference; new strings can be shortened with Huffman coding. HPACK deliberately avoids
general-purpose compression such as DEFLATE, which SPDY used for headers: the CRIME attack showed
that this could leak secret values such as cookies.

### Server push: do not rely on it

HTTP/2 lets a server **push** responses the client has not asked for yet, such as a page's CSS. It
was hard to use well: servers often pushed files the browser already had in its cache. Chrome
turned off HTTP/2 server push by default in 2022. To tell the browser early about important files, use
`<link rel="preload">` or the `103 Early Hints` status code instead.

### TLS and h2c

Browsers only use HTTP/2 over TLS. Client and server agree on it during the TLS handshake, with an
extension called **ALPN** (protocol name `h2`). Unencrypted HTTP/2, called **h2c**, is mostly used
between servers, for example for gRPC inside a private network.

## The problem HTTP/2 could not fix: TCP head-of-line blocking

HTTP/2 removed head-of-line blocking at the *HTTP* level. But all streams still share one TCP
connection, and TCP delivers bytes **strictly in order**. If one packet is lost, TCP holds back every
byte after it until the packet is retransmitted, even bytes for other streams that arrived fine.

```text
A packet carrying part of stream 3 is lost.

HTTP/2 over TCP:   stream 1: waiting   stream 3: waiting   stream 5: waiting
                   (TCP holds back ALL later bytes until the lost packet is resent)

HTTP/3 over QUIC:  stream 1: delivered stream 3: waiting   stream 5: delivered
                   (only the stream that lost data waits)
```

On a clean wired network this rarely matters. On a lossy mobile network HTTP/2 can be slower than
HTTP/1.1, where a lost packet stalls only one of six connections. Fixing this meant changing the
transport, not HTTP.

## HTTP/3: HTTP over QUIC

TCP lives in operating system kernels and is inspected by many routers and firewalls, so it is very
hard to change. **QUIC** (RFC 9000) is a new transport **on top of UDP**, with reliability,
congestion control, streams and TLS 1.3 encryption. It is usually implemented **in user space**: in
a library inside the application, not in the operating system kernel. HTTP/3 is HTTP mapped onto
QUIC.

```text
   HTTP/1.1          HTTP/2             HTTP/3
 +-----------+    +-----------+    +-----------+
 | HTTP text |    |  HTTP/2   |    |  HTTP/3   |
 |           |    |  frames   |    |  frames   |
 +-----------+    +-----------+    +-----------+
 | TLS (opt.)|    |    TLS    |    |   QUIC    |  streams, reliability,
 +-----------+    +-----------+    | + TLS 1.3 |  congestion control
 |    TCP    |    |    TCP    |    +-----------+
 +-----------+    +-----------+    |    UDP    |
 |    IP     |    |    IP     |    +-----------+
 +-----------+    +-----------+    |    IP     |
                                   +-----------+
```

### Independent streams and QPACK

QUIC knows about streams, so a lost packet only delays its own stream (see the diagram above).

HPACK assumes that the headers of all streams arrive in one fixed order. Over QUIC, streams are
independent, so this is no longer true. HTTP/3 therefore uses **QPACK** (RFC 9204), a variant that
works when streams arrive out of order.

### Faster connection setup

QUIC combines the transport handshake and the TLS 1.3 handshake:

| Setup | Round trips before the first request is sent |
|---|---|
| TCP + TLS 1.2 | 3 |
| TCP + TLS 1.3 | 2 |
| QUIC, new connection | 1 |
| QUIC, resumed connection with 0-RTT | 0 |

These numbers are for full handshakes. With TLS 1.2, optimisations such as session resumption or
"False Start" can save one round trip.

A **round trip (RTT)** is the time for a packet to reach the server and an answer to come back.
With a 100 ms round trip, each round trip saved is 100 ms saved on every new connection.

### 0-RTT and the replay problem

With **0-RTT** ("zero round trip"), a returning client sends its first request in its very first
packet, using keys from the earlier session (TLS 1.3 over TCP has the same feature). The catch: an
attacker who records that packet can **send it again**, and the server may process it twice.

> [!WARNING]
> Only accept 0-RTT ("early data") for requests that are safe to repeat, such as a `GET` that changes
> nothing. RFC 8470 defines an `Early-Data: 1` header that proxies add when they forward such a
> request, and a `425 Too Early` status that tells the client to retry after the full handshake.
> See [idempotency](/posts/retries-timeouts-and-idempotency) for why "safe to repeat" matters.

### Connection migration

A TCP connection is identified by client IP, client port, server IP and server port. When a phone
moves from Wi-Fi to mobile data, its IP changes and every TCP connection breaks. QUIC identifies a
connection by a **connection ID** instead, so the connection can continue from the new network after
a quick check of the new address. Behind a load balancer, this only works if the load balancer sends
packets with the same connection ID to the same server, even when the client's address changes.

### Discovery and fallback

A browser cannot know in advance that a server speaks HTTP/3, so the first visit usually uses TCP.
The server advertises HTTP/3 in a response header:

```http
Alt-Svc: h3=":443"; ma=86400
```

This means "this site is also available over HTTP/3 on UDP port 443; remember this for 86,400
seconds (one day)". Later connections try QUIC. The newer **HTTPS DNS record** (RFC 9460) can
announce HTTP/3 support before the first connection (see
[DNS for backend developers](/posts/dns-for-backend-developers)).

Some corporate networks block or throttle UDP. Browsers then **fall back to HTTP/2 or HTTP/1.1 over
TCP**. This is why servers offer HTTP/3 next to HTTP/1.1 and HTTP/2 over TCP, not instead of them.

## Trade-offs: when a newer version does not help

- **Inside a data centre**, round trips are short and loss is rare. HTTP/1.1 with keep-alive and a
  [connection pool](/posts/connection-pooling) is fine for most service-to-service traffic.
- **QUIC usually costs more CPU** for the same traffic. It runs in user space and handles every
  packet itself, while TCP benefits from decades of kernel and network card optimisations.
- **Debugging is harder.** QUIC encrypts most transport details that TCP sends in plain text (such
  as acknowledgements), so packet captures show little without the TLS keys.
- **Large downloads** are limited by bandwidth. The version matters most for many small requests
  over high-latency or lossy networks.

## What it means for backend developers

### Know where each protocol actually runs

The protocol your browser shows is only the **first hop**. A typical setup:

```text
browser --h3 or h2--> CDN / edge --h2 or h1.1--> load balancer --h1.1 or h2c--> app server
          (internet)              (internet or private)          (private network)
```

Each hop negotiates its own version. Your application may never see HTTP/2 or HTTP/3, and that is
usually fine.

### gRPC requires HTTP/2

gRPC is defined on top of HTTP/2. It uses streams for streaming calls and HTTP **trailers** (headers
sent after the body) for the final status. Every proxy between a gRPC client and server must speak
HTTP/2 towards the backend too. Browser JavaScript cannot read trailers, which is why browsers use a
variant called gRPC-Web, usually with a proxy (such as Envoy) that translates it to normal gRPC (see
[REST, gRPC and GraphQL](/posts/rest-grpc-graphql)).

### Balance requests, not connections

With HTTP/2, a client may send all its requests over **one long-lived connection**. A layer 4 load
balancer picks a backend once per connection, so that client's whole load lands on one server, and
newly added servers get nothing from existing connections. Fixes:

- Use a **layer 7** load balancer or proxy (Envoy, a service mesh, a cloud HTTP load balancer) that
  balances each request. See [load balancing](/posts/load-balancing-and-stateless-servers).
- Or use **client-side load balancing**: the client keeps connections to several backends.
- Limit connection lifetime. Many gRPC server libraries have a "max connection age" setting (for
  example `MaxConnectionAge` in gRPC-Go): after that time the server sends a `GOAWAY` frame, and the
  client opens a new connection, possibly to another backend.

### Enable HTTP/3 at the edge

Turn HTTP/3 on where TLS terminates, not in your application. Most CDNs offer it as a setting,
Caddy enables it by default, and nginx supports it from version 1.25. The nginx example below needs
nginx 1.25.1 or newer (for the `http2 on;` line), built with the HTTP/3 module
(`--with-http_v3_module`):

```nginx
server {
    listen 443 ssl;              # TCP: HTTP/1.1 and HTTP/2
    listen 443 quic reuseport;   # UDP: HTTP/3 (put "reuseport" in only one server block)
    http2 on;

    ssl_certificate     /etc/ssl/example.com.crt;
    ssl_certificate_key /etc/ssl/example.com.key;

    # Tell browsers that HTTP/3 is available on UDP port 443, for one day.
    add_header Alt-Svc 'h3=":443"; ma=86400' always;

    location / {
        proxy_pass http://app_servers;   # an "upstream app_servers" block defined elsewhere
    }
}
```

Then **open UDP port 443** in your firewall and cloud security groups. Test with
`curl -I --http3 https://example.com` (your curl must be built with HTTP/3 support). The first line
of the answer should start with `HTTP/3`: newer curl versions quietly fall back to an older version
if HTTP/3 fails, so check that line. You can also use the Protocol column in browser developer tools;
reload the page, because the first visit usually uses TCP. See
[reverse proxies and TLS](/posts/reverse-proxies-and-tls) for the rest of the proxy setup.

### Retire the HTTP/1.1 hacks

With HTTP/2 or HTTP/3, **domain sharding hurts**: it forces extra connections and handshakes.
Sprites and inlining make caching worse. Bundling is still useful in moderation (thousands of tiny
files still have overhead), but a few medium chunks are better than one huge bundle.

### Match keep-alive timeouts

This is about the hop between the load balancer and your app. If your app closes an idle keep-alive
connection at the moment the load balancer sends a new request on it, the user gets a random `502`.
Make the **app's keep-alive timeout longer than the load balancer's idle timeout**, so the load
balancer is always the side that closes first. For example, a Node.js HTTP server closes idle
connections after 5 seconds by default (`server.keepAliveTimeout`), while an AWS Application Load
Balancer has a default idle timeout of 60 seconds. With these two defaults, raise the Node.js value
above 60 seconds.

## Common mistakes

- **Assuming `h2` in the browser means HTTP/2 end to end.** It usually stops at the CDN or proxy.
- **Using a layer 4 load balancer for gRPC.** One backend gets hot while others sit idle.
- **Enabling HTTP/3 but forgetting UDP 443.** Everything still works over TCP, so nobody notices.
- **Allowing 0-RTT for non-idempotent requests.** A replayed `POST` can create a second order.
- **Running an old HTTP/2 implementation.** Attackers have abused HTTP/2 features, for example the
  "Rapid Reset" denial-of-service attack disclosed in 2023 (CVE-2023-44487), in which a client opens
  and immediately cancels very large numbers of streams. Keep proxies and servers patched.
- **Expecting HTTP/3 to fix a slow backend.** It saves network round trips, not database time (see
  [profiling](/posts/profiling-and-performance-debugging)).

## Further reading

- [High Performance Browser Networking](https://hpbn.co/) by Ilya Grigorik — free chapters on TCP,
  TLS, HTTP/1.1 and HTTP/2
- RFC 9113: [HTTP/2](https://www.rfc-editor.org/rfc/rfc9113)
- RFC 9114: [HTTP/3](https://www.rfc-editor.org/rfc/rfc9114)
- RFC 9000: [QUIC: A UDP-Based Multiplexed and Secure Transport](https://www.rfc-editor.org/rfc/rfc9000)
- RFC 8470: [Using Early Data in HTTP](https://www.rfc-editor.org/rfc/rfc8470)
