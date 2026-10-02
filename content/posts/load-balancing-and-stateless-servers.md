+++
title = "Load balancing and stateless servers: how one app becomes many"
summary = "Vertical vs horizontal scaling, why servers must be stateless to scale out, L4 vs L7 load balancers, the main balancing algorithms, health checks, connection draining and sticky sessions."
tags = ["scalability", "networking", "system-design", "backend"]
level = "beginner"
date = 2026-10-02
+++

Your app runs on one server. Traffic grows; the CPU sits at 90%. You can buy a bigger server, or run
several copies of the app behind a load balancer. The second option is how almost every large system
scales — but it only works if your application is built the right way. This article explains both
the load balancer and the "right way".

## Vertical vs horizontal scaling

- **Vertical scaling (scale up):** a bigger machine — more CPU, RAM, faster disk. Zero code changes,
  and modern servers are huge. But there is a ceiling, cost grows faster than capacity, and one machine
  is still one point of failure.
- **Horizontal scaling (scale out):** more machines running the same code. Practically unlimited,
  survives individual failures, lets you deploy one instance at a time — but requires a load balancer
  and **stateless** application servers.

Most systems do both: scale the stateless app tier horizontally, and scale the database vertically for
as long as possible (it is the hard part to scale out — see
[sharding](/posts/sharding-and-partitioning)).

## Stateless servers: the prerequisite

A server is **stateless** if any instance can handle any request, because nothing a request needs is
stored only in one instance's memory or disk. Things that commonly break this:

| State kept on the server | Problem | Move it to |
|---|---|---|
| Sessions in memory | User logged out when routed to another instance | Database or Redis session store, or signed cookies |
| Uploaded files on local disk | File missing on other instances, lost on redeploy | Object storage (S3-compatible) |
| In-memory caches as source of truth | Instances disagree | Shared cache, with the DB as source of truth |
| Scheduled jobs in every instance | Job runs N times | A job queue or a lock — see [background jobs](/posts/background-jobs-and-cron) |
| WebSocket connections | Inherently tied to one instance | Accept it, and route messages via pub/sub — see [scaling WebSockets](/posts/scaling-websockets-chat) |

The "twelve-factor app" methodology summarises this as: processes are stateless and share-nothing;
anything that must persist goes to a backing service (database, cache, object store).

This very site is stateless: sessions and votes live in PostgreSQL, so you can run as many copies as
you like behind a load balancer.

## What a load balancer does

```text
                         +--> app instance 1  (healthy)
 clients --> [ load  ] --+--> app instance 2  (healthy)
             [balancer]  +--> app instance 3  (failing health check: no traffic)
```

1. Accepts client connections on one address.
2. Picks a healthy backend for each connection or request.
3. Stops sending traffic to backends that fail health checks.
4. Often also: terminates TLS, adds `X-Forwarded-For`, compresses, limits rates, routes by path.

Examples: nginx, HAProxy, Envoy, Caddy, Traefik; cloud load balancers (AWS ALB/NLB, Google Cloud Load
Balancing, Azure Load Balancer); Kubernetes Services and Ingress controllers.

## Layer 4 vs layer 7

The "layer" refers to the OSI network model:

- **L4 (transport) load balancers** see TCP/UDP connections — IPs and ports — and forward bytes
  without understanding them. Very fast, protocol-agnostic. The backend is chosen once per
  connection.
- **L7 (application) load balancers** understand HTTP: they can route `/api/*` to one pool and
  `/static/*` to another, route by host name or header, retry failed requests, terminate TLS, and
  balance **per request** even when many requests share one connection.

Per-request balancing matters for HTTP/2 and gRPC: they multiplex many requests over one long-lived
connection, so an L4 balancer would send all of them to one backend.

## Balancing algorithms

| Algorithm | How it works | Good when |
|---|---|---|
| Round robin | Next backend in turn | Identical backends, similar requests |
| Weighted round robin | Bigger backends get proportionally more | Mixed instance sizes, canary releases (5% to new version) |
| Least connections | Backend with fewest active connections | Requests of very different duration, long-lived connections |
| Least response time / EWMA | Backend with the best recent latency | Heterogeneous or noisy backends |
| Power of two choices | Pick 2 random backends, use the less loaded | Many balancers with partial information; avoids herding |
| IP hash / consistent hashing | Same client or key → same backend | Cache affinity, sharded in-memory state |
| Random | Pick at random | Simple, surprisingly good at scale |

Start with round robin or least connections. "Power of two random choices" is worth knowing: it gets
most of the benefit of always picking the least-loaded backend without every balancer stampeding to
the same "best" backend at once.

## Health checks

The balancer periodically calls each backend (e.g. `GET /healthz`) and removes ones that fail
repeatedly. Two kinds are often distinguished (Kubernetes makes this explicit):

- **Liveness** — "is the process alive, or should it be restarted?" Keep it simple; a liveness check
  that depends on the database will restart all your instances when the database blips.
- **Readiness** — "should this instance receive traffic right now?" May check critical dependencies
  and returns failure during startup and shutdown.

## Connection draining and graceful shutdown

When an instance is removed (deploy, scale-in), in-flight requests should finish:

1. The instance is marked "not ready"; the balancer stops sending *new* requests.
2. The process receives `SIGTERM`, stops accepting new connections, finishes in-flight requests, closes
   database connections, and exits.
3. After a timeout, anything left is killed.

Without this, every deploy produces a burst of errors. (This site's server handles `SIGTERM` exactly
this way — see `shutdown_signal` in `src/main.rs`.)

## Sticky sessions: usually a smell

**Session affinity** makes the balancer send the same client to the same backend (via a cookie or IP
hash). It's a workaround for stateful servers, and it hurts: load becomes uneven, and when that
backend dies its users lose their state anyway. Prefer making servers stateless. Legitimate uses exist
(caching affinity, long-lived connections), but treat stickiness as an optimisation, never as a place
to keep the only copy of data.

## Don't let the load balancer be the new single point of failure

- Cloud load balancers are managed and redundant by design.
- Self-hosted: run two or more balancers with a floating virtual IP (keepalived/VRRP), or several
  balancers behind DNS.
- Globally: **DNS-based** or **anycast** load balancing spreads users across regions; GeoDNS sends users
  to the nearest one.

## Knowing the real client IP

Behind a balancer, the TCP peer of your app is the balancer, not the user. The balancer adds the
client address to `X-Forwarded-For`. Only trust the entries added by *your* proxies — counting from the
right — because clients can send a fake header. This site does exactly that for its IP-based vote
limits (`TRUSTED_PROXY_HOPS`, see `src/ip.rs`).

## Key takeaways

- Scale the app tier **horizontally**; that requires **stateless** servers.
- Move sessions, files and scheduled jobs out of instance memory and disk.
- Use **L7** balancing for HTTP/2 and gRPC; pick round robin or least connections to start.
- Health checks, graceful shutdown and connection draining turn deploys into non-events.
- Sticky sessions are a workaround, not a design.

## Further reading

- [The Twelve-Factor App](https://12factor.net/) — especially "Processes" and "Disposability"
- NGINX docs: [HTTP Load Balancing](https://docs.nginx.com/nginx/admin-guide/load-balancer/http-load-balancer/)
- Kubernetes docs: [Liveness, Readiness and Startup Probes](https://kubernetes.io/docs/concepts/configuration/liveness-readiness-startup-probes/)
- Michael Mitzenmacher, *The Power of Two Choices in Randomized Load Balancing*
