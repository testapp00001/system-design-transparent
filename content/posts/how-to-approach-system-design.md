+++
title = "How to approach a system design problem (with back-of-the-envelope math)"
summary = "A repeatable process for designing systems — in interviews and at work: clarify requirements, estimate load with simple arithmetic, sketch the API and data model, build the high-level design, then dive into the bottlenecks."
tags = ["system-design", "backend", "scalability"]
level = "beginner"
date = 2026-10-02
+++

"Design a URL shortener." "Design a chat app." Whether it's an interview or a real project kickoff,
staring at a blank whiteboard is hard. Experienced engineers don't have a magic answer — they have a
**process**. This article gives you one, plus the arithmetic that makes your design grounded in numbers
instead of buzzwords.

## Step 1: Clarify requirements (don't skip this)

Most bad designs solve the wrong problem. Spend the first minutes asking questions.

**Functional requirements** — what the system does:

- Who are the users, and what are the core actions? (For a URL shortener: create a short link;
  redirect; maybe custom aliases, expiry, analytics.)
- What's explicitly *out* of scope?

**Non-functional requirements** — how well it must do it:

- **Scale**: how many users? Requests per second? Data volume and growth?
- **Latency**: what's acceptable for each key operation?
- **Availability**: what happens if it's down for an hour? (A redirect service that's down breaks every
  link ever shared.)
- **Consistency**: can a newly created link take a few seconds to work everywhere? Is anything here
  money?
- **Read/write ratio**: a shortener might be 100 reads per write.

Write the answers down. Every later decision should trace back to one of them.

## Step 2: Back-of-the-envelope estimation

You don't need precise numbers — you need the **order of magnitude**, to know whether you're designing
for one server or a thousand. A few facts make this fast:

```text
1 day ≈ 86,400 s  ≈ 10^5 s   (use 100k for quick math)
1 million requests/day  ≈ 12 requests/s on average
peak ≈ 2–10× average (depends on the product's daily pattern)
```

Approximate latencies worth memorising (orders of magnitude; real numbers vary by hardware):

| Operation | Time |
|---|---|
| Main memory reference | ~100 ns |
| Read 1 MB sequentially from memory | ~10 µs (or less) |
| Round trip within a data center | ~0.5 ms |
| Random read from an NVMe SSD | ~0.1 ms |
| Read 1 MB sequentially from SSD | ~0.5–1 ms |
| Simple indexed database query (cached) | ~1 ms |
| Round trip between continents | ~100–150 ms |

Rough capacity of single components (very workload-dependent, but useful for sanity checks):

- One app server: hundreds to thousands of simple requests/s.
- One well-tuned PostgreSQL/MySQL primary: thousands to tens of thousands of simple queries/s.
- Redis: on the order of 100,000 simple operations/s per instance.

### Worked example: a URL shortener

Assume 100 million new links per month and 100:1 read/write ratio.

```text
writes:  100M / month ≈ 100M / (30 × 10^5 s) ≈ 33 writes/s       (peak, say 100/s)
reads:   33 × 100 ≈ 3,300 redirects/s                            (peak, say 10,000/s)
storage: 100M links/month × 12 months × 5 years = 6 billion links
         × ~500 bytes per link (URL, code, metadata, index overhead) ≈ 3 TB over 5 years
short code length: base62 (a–z, A–Z, 0–9): 62^7 ≈ 3.5 trillion codes -> 7 characters is plenty
```

What this tells you: writes are trivial for one database; reads at 10,000/s are fine for a cache in
front of a database; 3 TB over five years fits on one large database or a few shards. No exotic
technology needed — the interesting parts are elsewhere (code generation, caching, availability of
redirects, abuse prevention).

## Step 3: API and data model

Sketch the main endpoints and the data they touch:

```http
POST /links            {"url": "https://..."}        -> {"code": "aZ3kL9q", "short_url": "..."}
GET  /{code}           -> 301/302 redirect
GET  /links/{code}/stats
```

```text
links: code (PK), long_url, owner_id, created_at, expires_at
clicks (if analytics): code, ts, country, referrer  -> high volume, append-only: different store
```

Choosing the data model early exposes questions: what's the primary access pattern (by code), what's
high-volume (clicks), what must be unique (code). See [choosing a database](/posts/choosing-a-database).

## Step 4: High-level design

Draw the boxes for the core flows, nothing more:

```text
 client --> [load balancer] --> [app servers (stateless)] --> [cache] --> [database]
                                         |
                                         +--> [queue] --> [analytics workers] --> [analytics store]
```

Walk through each functional requirement on the diagram: "a redirect hits the load balancer, an app
server checks the cache by code, falls back to the database, responds 301…". If a requirement doesn't
map onto the diagram, the diagram is incomplete.

## Step 5: Deep dives into the hard parts

Pick the two or three areas that matter most for *this* problem, guided by the requirements and the
numbers. For the shortener:

- **Generating unique codes**: hash of the URL (collisions; same URL → same code — is that desired?),
  random codes with a uniqueness check (a unique index and retry on conflict), or encoding a
  distributed counter in base62 (predictable — maybe fine, maybe a privacy issue).
- **Read path at scale**: cache-aside with long TTLs (links rarely change), CDN caching of redirects,
  301 (browsers cache it, fewer hits, worse analytics) vs 302.
- **Analytics without slowing redirects**: publish click events asynchronously to a queue; aggregate
  in a separate store.
- **Abuse**: rate-limit creation, scan for malicious URLs.

Typical deep-dive topics in other problems: fan-out in feeds and chat (see
[scaling WebSockets](/posts/scaling-websockets-chat)), consistency for money, hot keys, sharding keys
(see [sharding](/posts/sharding-and-partitioning)), idempotency of payments (see
[idempotency](/posts/retries-timeouts-and-idempotency)).

## Step 6: Bottlenecks, failures and trade-offs

Finish by stress-testing your own design:

- What's the **single point of failure**? (One database primary? One region?)
- What happens when the **cache is cold** or down?
- Where does it break at **10× the load**?
- What did you **trade away**, and why was that the right call for these requirements?

Saying "I chose X, which costs Y, because requirement Z matters more" is the core skill being tested —
in interviews and in real design reviews.

## Habits that make designs better

- **Numbers before boxes.** Estimates prevent both over-engineering and under-engineering.
- **Start simple, then evolve.** Show the design that works today, then how it grows.
- **Name the trade-offs** explicitly; there are no free choices.
- **Know the building blocks** — load balancers, caches, queues, replicas, shards, CDNs — and what each
  costs. The [roadmap](/roadmap) lists them.
- **Write it down**: a one-page design doc with context, requirements, the decision, alternatives
  considered and their trade-offs is worth more than a beautiful diagram.

## Further reading

- Alex Xu, *System Design Interview – An Insider's Guide* (volumes 1 and 2)
- [The System Design Primer](https://github.com/donnemartin/system-design-primer) (GitHub)
- Jeff Dean's latency numbers, interactive: [Latency Numbers Every Programmer Should Know](https://colin-scott.github.io/personal_website/research/interactive_latency.html)
- Martin Kleppmann, *Designing Data-Intensive Applications*
