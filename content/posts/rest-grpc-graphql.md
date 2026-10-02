+++
title = "REST vs gRPC vs GraphQL: which API style, and when?"
summary = "Do internal services need gRPC? Should a new API start with GraphQL? A practical comparison of the three styles — how they work, what they cost, and sensible defaults."
tags = ["api-design", "microservices", "backend"]
level = "intermediate"
date = 2026-10-02
+++

Two questions come up on almost every new project: *"Should our services talk gRPC?"* and *"Should we
build the API in GraphQL from the start?"*. The honest answer to both is "it depends", so this article
spells out what it depends on.

## REST (resource-oriented HTTP + JSON)

Resources are identified by URLs; HTTP methods express intent; JSON is the usual format.

```http
GET /users/42/orders?status=paid&limit=20
POST /orders            {"items": [...]}
DELETE /orders/1042
```

**Strengths**

- Universal: every language, tool, proxy, browser and engineer understands it. Debug with `curl`.
- Uses HTTP's features for free: caching (`Cache-Control`, `ETag`), status codes, content negotiation,
  standard idempotency semantics, CDNs.
- Loose coupling: clients and servers evolve independently if you follow additive changes.

**Weaknesses**

- **Over-fetching / under-fetching**: endpoints return fixed shapes. A mobile screen may need 3 calls
  (user, orders, recommendations) or get 50 fields when it needed 5.
- No enforced contract by default — use **OpenAPI** to describe it and generate clients.
- JSON is verbose and relatively slow to parse at very high volumes.

## gRPC (RPC over HTTP/2 + Protocol Buffers)

You define services and messages in a `.proto` file; code generators produce typed clients and server
stubs in many languages.

```protobuf
service OrderService {
  rpc GetOrder(GetOrderRequest) returns (Order);
  rpc StreamOrderUpdates(OrderFilter) returns (stream OrderEvent);
}
message GetOrderRequest { int64 id = 1; }
```

**Strengths**

- **A strict, typed contract** with generated code — calls look like local functions, and breaking
  changes are visible at compile time.
- **Efficient**: binary Protobuf encoding is compact and fast; HTTP/2 multiplexes many calls over one
  connection.
- **Streaming** in both directions is first-class.
- Built-in conventions for **deadlines** (timeouts propagated across services), cancellation, status
  codes and metadata.

**Weaknesses**

- **Browsers can't call gRPC directly**; you need gRPC-Web (with a proxy) or a translation layer such
  as gRPC-Gateway or Connect.
- Harder to debug by hand (binary payloads; use `grpcurl` and reflection).
- HTTP/2 long-lived connections complicate load balancing: an L4 load balancer pins all calls of a
  connection to one backend. You need L7 (per-request) balancing — e.g. Envoy, a service mesh, or
  client-side balancing.
- Schema evolution rules must be learned (never reuse field numbers; add fields, don't change types).

## GraphQL (one endpoint, client-chosen shape)

The server publishes a typed schema; the client sends a query describing exactly the data it wants.

```graphql
query {
  user(id: 42) {
    name
    orders(last: 5) { id total status }
    recommendations(limit: 3) { title }
  }
}
```

**Strengths**

- **Clients get exactly what they need in one round trip** — great for complex UIs and mobile apps on
  slow networks.
- A typed schema with introspection and excellent tooling.
- A good fit as an **aggregation layer** over many backend services (a "BFF" or federated graph).
- Lets frontend teams iterate without waiting for new endpoints.

**Weaknesses**

- **Performance traps**: naive resolvers cause the **N+1 problem** (one query for 50 orders, then 50
  queries for their items). You need batching (DataLoader) from day one.
- **Security and cost control**: clients can write arbitrarily deep or expensive queries. You need
  depth/complexity limits, timeouts, and ideally persisted (allow-listed) queries.
- **HTTP caching mostly disappears** (everything is a POST to `/graphql`); caching moves to the
  client and to custom server layers.
- More moving parts: schema design, resolvers, authorization per field, observability per resolver.

## Side by side

| | REST | gRPC | GraphQL |
|---|---|---|---|
| Typical use | Public APIs, web backends | Service-to-service | Complex client UIs, aggregation |
| Contract | Optional (OpenAPI) | Required (.proto) | Required (schema) |
| Payload | JSON (text) | Protobuf (binary) | JSON |
| Browser support | Native | Via gRPC-Web/proxy | Native |
| HTTP caching | Excellent | None | Limited |
| Streaming | SSE/WebSockets alongside | Native, bidirectional | Subscriptions (extra infra) |
| Learning curve | Low | Medium | Medium–high |
| Main risk | Chatty clients, drift without a spec | Tooling and LB complexity | N+1 and expensive queries |

## Answering the two questions

### "Do our internal services need gRPC?"

Consider gRPC when **several** of these are true:

- You have many services in several languages and want generated, typed clients.
- Calls are very frequent and latency- or bandwidth-sensitive.
- You need streaming between services.
- You're ready to run L7 load balancing (a mesh or Envoy) and learn the tooling.

Stay with REST/JSON (with an OpenAPI spec) when you have a handful of services, mostly one language,
moderate traffic, or a team new to distributed systems. JSON over HTTP is *not* the bottleneck for
most companies — database queries are. And remember that much service-to-service communication
should be **asynchronous** messages rather than calls of any style (see
[queues and streams](/posts/message-queues-and-event-streams)).

### "Should we build GraphQL from the start?"

Usually **no**, unless:

- Your clients are **many and diverse** (web, iOS, Android, partners) with different data needs, or
- Your UI composes data from **many backend services**, or
- You have people who have run GraphQL in production before.

For a typical first version — one web frontend, one backend, one team — REST (or even server-rendered
HTML, like this site) is faster to build, easier to secure and easier to cache. You can add a GraphQL
layer later *on top of* existing services when the over-/under-fetching pain is real. Going the other
way is harder.

## They're not mutually exclusive

A common, healthy architecture:

```text
 browsers / mobile  --REST or GraphQL-->  API gateway / BFF  --gRPC or REST-->  internal services
 partners           --REST (public, versioned, documented)-->                    (+ async events)
```

Public APIs favour REST for reach and stability; the client-facing aggregation layer may be GraphQL;
internal high-traffic calls may be gRPC; everything else is events.

## Whatever you choose

- **Write the contract down** (OpenAPI, `.proto`, GraphQL schema) and generate clients from it.
- **Make changes additive**; deprecate before removing. See [API design](/posts/api-design-pagination-versioning).
- **Set timeouts and retries deliberately.** See [retries and idempotency](/posts/retries-timeouts-and-idempotency).
- **Paginate every list.**

## Further reading

- [gRPC documentation](https://grpc.io/docs/) — core concepts, deadlines, load balancing
- [GraphQL Learn](https://graphql.org/learn/) — including [performance](https://graphql.org/learn/performance/) and [security](https://graphql.org/learn/security/)
- [OpenAPI Specification](https://spec.openapis.org/oas/latest.html)
- Roy Fielding's dissertation, [chapter 5: REST](https://ics.uci.edu/~fielding/pubs/dissertation/rest_arch_style.htm)
