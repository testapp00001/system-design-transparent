+++
title = "Reverse proxies and TLS termination: nginx, Caddy, HAProxy and Envoy"
summary = "What the proxy in front of your app really does, how certificates, SNI, the TLS 1.3 handshake and automatic renewal work, correct nginx and Caddy configs for headers, WebSockets and SSE, and how to choose a proxy."
tags = ["networking","devops","security"]
level = "intermediate"
date = 2026-10-02
+++

Your app listens on port 8080 and works on your laptop. Now it must go on the internet. Browsers
expect HTTPS on port 443, so you need a certificate, and it expires every few months. You also want
fast static files, an upload size limit, and a second app instance for deploys. Almost nobody builds
this into the app. They put a **reverse proxy** in front: a server that receives every client
request and forwards it to the app. This article explains what it does, how TLS works inside it,
and how to configure it without the usual mistakes.

## What a reverse proxy does

A *forward* proxy acts for clients, like a company proxy for employee browsers. A *reverse* proxy
acts for servers: clients talk to it as if it were your site, and it talks to your app.

```text
                     +----------------------------------+
   browser           |  reverse proxy (nginx, Caddy...) |         your apps (private network)
  ---- HTTPS ------> |                                  | -- HTTP --> api.example.com: api-1, api-2
  port 443           |  1. terminate TLS (cert + key)   | -- HTTP --> example.com/: web-1
                     |  2. route by host and path       |
                     |  3. limits, compression, buffers | -- disk --> example.com/static/*
                     |  4. add X-Forwarded-* headers    |
                     +----------------------------------+
```

Its usual jobs:

- **TLS termination.** The proxy holds the certificate and private key, decrypts HTTPS, and forwards
  plain HTTP to the app. The app never deals with TLS.
- **Routing** by host name (`api.example.com`) or path (`/api/*`). One IP address can serve many sites.
- **Load balancing** over several app instances, skipping unhealthy ones (see
  [load balancing](/posts/load-balancing-and-stateless-servers)).
- **Compression** (gzip, Brotli, zstd) of text responses.
- **Buffering.** The proxy reads the whole request first, and reads the app's response quickly,
  then sends it to slow clients at their speed, so the app worker is free after milliseconds.
  Gunicorn's docs, for example, strongly recommend a buffering proxy such as nginx in front of its
  default synchronous workers.
- **Static files** served straight from disk.
- **Limits** on body size, connections and request rate (see [rate limiting](/posts/rate-limiting)).
- **Forwarded headers.** The app's network peer is now the proxy, so the proxy reports the real
  client: `X-Forwarded-For` (client IP), `X-Forwarded-Proto` (`http` or `https`) and
  `X-Forwarded-Host`. RFC 7239 standardises a `Forwarded` header, but the `X-` versions are more
  common.

Load balancers, API gateways and CDNs are reverse proxies too.

## TLS in a few minutes

TLS (Transport Layer Security) gives a connection **encryption** (nobody on the path can read it),
**integrity** (nobody can change it unnoticed) and **authentication** (you are really talking to
`example.com`). HTTPS is HTTP inside TLS.

### Certificates and chains

A **certificate** says "this public key belongs to `example.com`, valid from date A to date B",
signed by a **certificate authority (CA)**. Your server keeps the matching **private key** secret.
Clients trust a built-in list of **root CAs**. Roots rarely sign server certificates directly; they
sign **intermediate** CAs, which sign yours (the "leaf"):

```text
root CA (already in the client's trust store) --signs--> intermediate CA --signs--> example.com
```

Your server must send the leaf **and the intermediates**: the "full chain". With only the leaf,
some browsers still work (they cached the intermediate from another site, or download it
themselves), but `curl`, mobile apps and other servers fail with errors like "unable to get local
issuer certificate". With certbot, use `fullchain.pem`.

### SNI: many sites on one IP address

In its first message, the client names the host it wants, using a TLS extension called **SNI**
(Server Name Indication). The proxy uses it to pick the right certificate before any HTTP is
exchanged. SNI is visible on the network; the newer Encrypted Client Hello (ECH) extension hides it,
but is not supported everywhere yet.

### The TLS 1.3 handshake

```text
client                                                            server
  |  ClientHello: versions, ciphers, key share,                      |
  |               SNI = example.com, ALPN = [h2, http/1.1]           |
  |----------------------------------------------------------------->|
  |  ServerHello: chosen cipher, server key share                    |
  |  (both sides now compute the same keys; the rest is encrypted)   |
  |  EncryptedExtensions (chosen ALPN), Certificate chain,           |
  |  CertificateVerify (signature), Finished                         |
  |<-----------------------------------------------------------------|
  |  client checks: chain ends at a trusted root, name, dates        |
  |  Finished + first HTTP request                                   |
  |----------------------------------------------------------------->|
```

Each side sends a **key share**: a fresh, one-time public key for a Diffie-Hellman-style key
exchange. From the two shares, both compute the same secret keys. The server proves it owns the
certificate by signing the handshake with its private key. That is **one round trip** before the
first request, instead of two for a full TLS 1.2 handshake. Because the keys are new for every
connection, recorded traffic stays safe even if the private key leaks later (**forward secrecy**).
ALPN picks HTTP/2 or HTTP/1.1 (see [HTTP versions](/posts/http-versions)). Leave TLS 1.3's "0-RTT"
mode (also called "early data") off unless you need it: an attacker can record 0-RTT data and send
it again (a replay), so it is only safe for requests that can run twice.

### Let's Encrypt, ACME and automatic renewal

Let's Encrypt is a free, automated CA. Programs talk to it with the **ACME** protocol (RFC 8555). To
get a certificate, you prove you control the domain by passing a **challenge**:

| Challenge | How you prove control | Notes |
|---|---|---|
| HTTP-01 | Serve a token at `http://<domain>/.well-known/acme-challenge/<token>` | Port 80 must be reachable |
| DNS-01 | Create a TXT record at `_acme-challenge.<domain>` | Required for wildcard certificates |
| TLS-ALPN-01 | Answer a special TLS handshake on port 443 | Supported by some proxies, such as Caddy |

Let's Encrypt certificates are valid for 90 days by default, and Let's Encrypt has announced that
it will lower this to 45 days by 2028. The CA/Browser Forum (browser makers and CAs who set the
rules) voted in 2025 (ballot SC-081) to shorten the maximum lifetime of all public certificates step
by step, down to 47 days from 2029. So renewal must be **automatic**. Caddy and Traefik renew by
themselves. With nginx or HAProxy you usually run an ACME client such as certbot, which renews well
before expiry and can reload the proxy. Both now also have newer built-in options: nginx's official
`ngx_http_acme_module` (released in 2025 as a separate module) and HAProxy's ACME client (since
HAProxy 3.2, still marked experimental). Renewal can still fail silently (a firewall closes port 80,
DNS moves), so monitor expiry from outside. Let's Encrypt stopped sending expiry reminder emails in
2025.

### HSTS: never come back over plain HTTP

Redirecting `http://` to `https://` leaves that first request unprotected. The **HSTS** header
tells the browser: "for the next N seconds, use only HTTPS for this host".

```http
Strict-Transport-Security: max-age=31536000; includeSubDomains
```

Start with a small `max-age` (86400 is one day), then raise it to a year. `includeSubDomains`
covers every subdomain, so check them all first. `preload` puts your domain in a list built into
browsers, which is very hard to undo.

## Configuring Caddy

Caddy gets and renews certificates itself, redirects HTTP to HTTPS, and has good proxy defaults:

```text
example.com {
    @compress not path /events/*
    encode @compress zstd gzip       # compress everything except the SSE stream

    handle_path /static/* {
        root * /srv/static
        file_server
    }

    handle {
        reverse_proxy app1:8080 app2:8080 {
            lb_policy  least_conn
            health_uri /healthz
        }
    }
}
```

With no more configuration, you get automatic HTTPS, an HTTP-to-HTTPS redirect, HTTP/3, working
WebSockets, and SSE (`text/event-stream`) responses that `reverse_proxy` flushes immediately. One
catch: `encode` also compresses `text/*` responses, and users have reported compressed SSE events
arriving late. So the example does not compress the `/events/` route. Caddy passes the original
`Host` header and sets `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Forwarded-Host`. It ignores
those headers when clients send them. If a CDN or load balancer sits in front of Caddy, list its IP
ranges with the `trusted_proxies` option, inside the `servers` block of the global options.

## Configuring nginx

nginx needs explicit settings, and its defaults surprise people. Out of the box it sends
`Host: app_servers` (the upstream name), not the host the client asked for. It does not pass on the
`Upgrade` and `Connection` headers that a WebSocket needs. And before version 1.29.7 (March 2026)
it spoke HTTP/1.0 to the backend, which cannot upgrade to WebSockets at all. Newer versions use
HTTP/1.1 and reuse backend connections (keep-alive) by default. The example below still sets
`proxy_http_version 1.1`, so it also works on older versions. It needs nginx 1.25.1 or later,
because of the `http2 on;` line (older versions write `listen 443 ssl http2;` instead).
A production-shaped example:

```nginx
# Included inside http { }, for example /etc/nginx/conf.d/example.conf
upstream app_servers {
    server 10.0.0.11:8080;
    server 10.0.0.12:8080;
}

# WebSockets: send "Connection: upgrade" only when the client asked to upgrade.
# For normal requests the value is empty, so nginx sends no Connection header
# and can keep the backend connection open for the next request.
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      '';
}

server {
    listen 80;
    server_name example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    http2 on;
    server_name example.com;

    ssl_certificate     /etc/letsencrypt/live/example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/example.com/privkey.pem;
    ssl_protocols       TLSv1.2 TLSv1.3;
    # HSTS: start with one day; later raise to 31536000 (one year)
    add_header Strict-Transport-Security "max-age=86400" always;

    client_max_body_size 20m;            # default is 1m; larger uploads get 413

    # Set once here, so every location below inherits all of them
    proxy_http_version 1.1;              # default only since nginx 1.29.7
    proxy_set_header Host              $host;
    proxy_set_header X-Forwarded-For   $remote_addr;   # we are the first proxy: overwrite
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Upgrade           $http_upgrade;
    proxy_set_header Connection        $connection_upgrade;
    proxy_connect_timeout 5s;
    proxy_read_timeout    60s;

    location / {
        proxy_pass http://app_servers;
    }

    location /ws/ {                      # WebSockets: allow long quiet periods
        proxy_pass http://app_servers;
        proxy_read_timeout 1h;
    }

    location /events/ {                  # Server-Sent Events: stream, don't buffer
        proxy_pass http://app_servers;
        proxy_buffering off;
        proxy_read_timeout 1h;
    }
}
```

- **`X-Forwarded-For`.** nginx is the first proxy here, so it *replaces* what the client sent.
  Behind a proxy you trust (a cloud load balancer), use `$proxy_add_x_forwarded_for`, which
  *appends*, and pass on the incoming `X-Forwarded-Proto`, because `$scheme` is then `http`.
- **Timeouts.** `proxy_read_timeout` (default 60 seconds) limits the time *between two reads* from
  the backend, not the whole response. A WebSocket quiet that long is closed, so raise it *and* send
  pings about every 30 seconds (for SSE, a comment line such as `: keep-alive`).
- **Streaming.** With buffering on, SSE events arrive in bursts. Turn it off for streaming routes,
  or have the app send the response header `X-Accel-Buffering: no`.
- **Reloading.** Check with `nginx -t`, then `nginx -s reload`. Old workers finish their requests.
  Open WebSocket and SSE connections keep an old worker alive until they close, unless you set
  `worker_shutdown_timeout`.

> [!WARNING]
> nginx inherits `proxy_set_header` (and `add_header`) from the outer level **only if the inner
> level defines none**. Add one `proxy_set_header` inside a `location`, and that location silently
> loses `Host` and every `X-Forwarded-*` header. Set them all at one level, or repeat all of them.

## Where TLS should end

| Option | How it works | Trade-off |
|---|---|---|
| Terminate at the edge | Proxy decrypts; plain HTTP to the app | Simplest. Fine on localhost or a private network you control |
| Re-encrypt | Proxy decrypts, then opens a new TLS connection to the app | Needed when traffic crosses networks you don't trust, or for compliance |
| Passthrough | A layer 4 (TCP-level) proxy routes by SNI without decrypting | The proxy sees no HTTP: no path routing, headers or compression |

## Mutual TLS between services

Normal TLS authenticates only the server. With **mutual TLS (mTLS)**, the client also presents a
certificate, and the server checks it. Between your services, this gives each one a strong identity
("this really is the billing service") and encrypts traffic even inside your network ("zero trust").
Certificates come from your **own internal CA**, and nginx can require them:

```nginx
server {
    listen 8443 ssl;
    ssl_certificate        /etc/nginx/tls/orders.crt;
    ssl_certificate_key    /etc/nginx/tls/orders.key;
    ssl_client_certificate /etc/nginx/tls/internal-ca.crt;  # the CA that signs clients
    ssl_verify_client      on;                              # no valid client cert: rejected
    # ... locations as before
}
```

`$ssl_client_s_dn` holds the subject (the name) of the verified client certificate, to pass to the
app. In the other direction, `proxy_ssl_certificate` and `proxy_ssl_certificate_key` make nginx
present its own certificate to a backend, and `proxy_ssl_verify` with
`proxy_ssl_trusted_certificate` makes it check the backend's certificate: nginx is then an mTLS client.

The hard part is issuing certificates to every service and rotating them. Prefer short-lived
certificates, because revoking a stolen one is difficult. With a few services, a small internal CA
such as step-ca or Vault's PKI engine is enough (see [secrets management](/posts/secrets-management)).
With many, a service mesh such as Istio (largely built on Envoy) or Linkerd does mTLS and rotation
for you. SPIFFE standardises these identities as URIs like `spiffe://example.org/billing`.

## nginx, Caddy, HAProxy, Envoy and Traefik compared

| | nginx | Caddy | HAProxy | Envoy | Traefik |
|---|---|---|---|---|---|
| Written in | C | Go | C | C++ | Go |
| Configuration | Text files | Caddyfile or JSON API | Text file, runtime API | YAML or xDS APIs from a control plane | Docker labels, Kubernetes, files |
| Automatic certificates | Usually external (certbot); newer official ACME module | Yes, by default | Usually external; experimental built-in client (3.2+) | Not built in | Yes |
| Serves static files | Yes, very well | Yes | No | No | No |
| Best at | General web server and proxy | Small setups, HTTPS that just works | Fast TCP and HTTP load balancing | Service-to-service traffic, gRPC, metrics | Containers that come and go |
| Watch out for | Header inheritance, surprising defaults | Rate limiting needs a plugin | Not a web server | Verbose; rarely written by hand | Many concepts (routers, middlewares) |

Rules of thumb: on one server, Caddy (see
[running production on a single VPS](/posts/deploy-on-a-single-vps)) or the nginx your team knows.
Containers that change often: Traefik, Caddy or your ingress controller. Heavy load balancing:
HAProxy. Many microservices with mTLS and gRPC: Envoy, usually via a mesh. On a cloud, the managed
load balancer may already terminate TLS.

## Trust forwarded headers only from your proxy

Any client can send `X-Forwarded-For` itself. Here is what reaches the app when a proxy appends:

```text
client sends:      X-Forwarded-For: 6.6.6.6                  (made up by the client)
proxy appends:     X-Forwarded-For: 6.6.6.6, 203.0.113.7
                                    ^ fake   ^ added by your proxy: the real client
```

An app that takes the **leftmost** value lets visitors choose their IP address. The rules:

1. **Make the app reachable only through the proxy.** Bind it to `127.0.0.1` or a private network
   and firewall its port. Otherwise attackers skip the proxy and send any header.
2. **Count from the right.** With N trusted proxies, the client is the Nth entry from the right
   (see [load balancing](/posts/load-balancing-and-stateless-servers)). This site's server does this
   with its `TRUSTED_PROXY_HOPS` setting, in `src/ip.rs`.
3. **Use your framework's setting** rather than parsing headers yourself: Express's `trust proxy`,
   Django's `SECURE_PROXY_SSL_HEADER` (for `X-Forwarded-Proto`), or nginx's `set_real_ip_from`
   behind another proxy.
4. **Treat `X-Forwarded-Proto` the same way.** Apps use it to build URLs, decide on redirects and
   mark cookies `Secure`.

## Checklist

- [ ] Only the proxy is public (ports 80 and 443); the app listens on a private address.
- [ ] Certificates renew automatically, and expiry is monitored from outside.
- [ ] Only TLS 1.2 and 1.3, with settings from the Mozilla SSL Configuration Generator.
- [ ] HTTP redirects to HTTPS; HSTS is on once everything works.
- [ ] Proxy timeouts match the app's timeouts and your [retry policy](/posts/retries-timeouts-and-idempotency).

## Common mistakes

- **Endless redirects after enabling HTTPS.** The app sees plain HTTP from the proxy and keeps
  redirecting to HTTPS. Pass `X-Forwarded-Proto` and trust it.
- **Every visitor has the proxy's IP,** so per-IP rate limits block everyone at once.
- **Serving only the leaf certificate.** It works in your browser and fails in `curl` and apps.
- **Mysterious `413`, `504`, or WebSockets dropping after a minute:** nginx's 1 MB body limit and
  60-second read timeout.
- **HSTS `preload` or a one-year `max-age` on day one,** before every subdomain supports HTTPS.
- **Private keys in Git** or readable by every user. Only the proxy should read them.

## Further reading

- RFC 8446: [The Transport Layer Security (TLS) Protocol Version 1.3](https://www.rfc-editor.org/rfc/rfc8446)
- Let's Encrypt: [Challenge Types](https://letsencrypt.org/docs/challenge-types/)
- nginx docs: [ngx_http_proxy_module](https://nginx.org/en/docs/http/ngx_http_proxy_module.html) and [WebSocket proxying](https://nginx.org/en/docs/http/websocket.html)
- Caddy docs: [reverse_proxy directive](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy) and [Automatic HTTPS](https://caddyserver.com/docs/automatic-https)
- Mozilla: [SSL Configuration Generator](https://ssl-config.mozilla.org/) (now maintained by the
  community TLSRef project; this link redirects there)
- RFC 7239: [Forwarded HTTP Extension](https://www.rfc-editor.org/rfc/rfc7239)
