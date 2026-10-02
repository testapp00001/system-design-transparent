+++
title = "Monolith vs microservices (and why monorepo is a different question)"
summary = "When splitting into services actually helps, what it costs, the 'distributed monolith' trap, the modular monolith middle ground — and why monorepo vs polyrepo is about code storage, not architecture."
tags = ["architecture", "microservices", "system-design"]
level = "intermediate"
date = 2026-10-02
+++

"We should move to microservices" is one of the most expensive sentences in software. Sometimes it is
exactly right. Often it trades problems you understand for problems you don't. This article explains
what each architecture optimises for, so you can tell which situation you are in.

First, a vocabulary fix that clears up a lot of confusion.

## Monorepo is not the opposite of microservices

These are **two independent decisions**:

- **Monolith vs microservices** — how the *running system* is split: one deployable application, or
  many independently deployed services talking over the network.
- **Monorepo vs polyrepo** — how the *source code* is stored: one Git repository for everything, or one
  repository per project.

All four combinations exist:

| | Monorepo | Polyrepo |
|---|---|---|
| **Monolith** | The common default | A monolith plus separate repos for shared libraries |
| **Microservices** | Google, Uber and many others: many services, one repo | One repo per service |

A monorepo makes cross-service refactors atomic (one commit changes the API and all its callers) and
keeps tooling consistent, but needs build tooling that only builds and tests what changed (Bazel,
Nx, Turborepo, Pants, Gradle). Polyrepos give teams independence but make cross-cutting changes slow
and dependency versions drift.

## What a monolith is (and isn't)

A **monolith** is one deployable unit: one process (often several identical copies behind a load
balancer), one codebase, usually one database. It is *not* automatically a "big ball of mud" — a
monolith can be very well structured inside (see [beyond MVC](/posts/beyond-mvc-project-architecture)).

**What monoliths make easy**

- Function calls instead of network calls: fast, reliable, typed, no serialisation.
- **Transactions** across any data — no distributed consistency problems.
- One thing to build, test, deploy, monitor and debug. A stack trace shows the whole story.
- Refactoring across module boundaries is an IDE operation.

**Where monoliths hurt (eventually)**

- Many teams in one codebase step on each other: merge conflicts, a shared release train, one team's
  bug blocks everyone's deploy.
- Everything scales together — the image-processing part that needs GPUs is deployed with the
  login page.
- One memory leak or CPU-hungry endpoint affects everything.
- Long build and test times as the codebase grows.

## What microservices are

Many small services, each **owning a business capability and its own data**, deployed independently,
communicating over the network (HTTP/gRPC calls and/or asynchronous events).

**What microservices make easy**

- **Team autonomy**: a team owns a service end to end and deploys on its own schedule.
- **Independent scaling** and resource profiles per service.
- **Fault isolation** — in theory: one service failing need not take down the others.
- Technology choice per service, where truly justified.

**What they cost** — the "microservice premium":

- **Network calls fail** in ways function calls don't: timeouts, retries, partial failure,
  idempotency. See [retries and idempotency](/posts/retries-timeouts-and-idempotency).
- **No cross-service transactions.** "Create order and reserve stock" now needs sagas, outboxes and
  eventual consistency. See [distributed transactions](/posts/distributed-transactions-saga-outbox).
- **Operational load**: N pipelines, N dashboards, service discovery, distributed tracing, versioned
  APIs, a platform team to run it all.
- **Debugging** spans many logs and processes; you need [tracing](/posts/observability-logs-metrics-traces).
- **Latency adds up**: a request fanning out to 10 services is only as fast as the slowest.

## The distributed monolith: worst of both worlds

The most common failure mode: services split along technical layers or arbitrary lines, sharing a
database, calling each other synchronously in long chains, and needing to be **deployed together**
because changes always span several of them. You pay all the costs of distribution and get none of the
independence.

Warning signs:

- A feature routinely needs coordinated changes and deploys in 3+ services.
- Services read and write each other's database tables.
- One service going down takes down most others (synchronous call chains).
- "Shared" libraries containing business logic that every service must upgrade in lockstep.

## The modular monolith: the underrated middle ground

Structure a single deployable application as **modules with strict boundaries**: each module owns its
tables, exposes a small public interface, and other modules may only use that interface (enforced by
code review, package visibility, or architecture tests). You keep in-process calls and transactions
while getting most of the organisational clarity of services.

Shopify is a well-known example: it kept its large Rails monolith and invested in "componentising"
it rather than splitting it into services. And when a module later *does* need to become a service,
the boundary is already there — extraction becomes a mechanical change instead of a rewrite.

## When does splitting become necessary?

Consider extracting a service when you can point to one of these concrete forces:

1. **Team scaling.** Several teams are blocked by each other's releases, and the boundaries between
   their work are clear and stable. (Conway's law: system structure mirrors communication structure.
   Split along team and business boundaries, not technical layers.)
2. **Different scaling or resource needs.** Video transcoding, ML inference, or a hot read path that
   needs 50 instances while the rest needs 3.
3. **Different reliability or security requirements.** Payments isolated for compliance (e.g. PCI
   scope), or a risky component that must not take down checkout.
4. **Different release cadence.** A component that changes 20 times a day next to one that must change
   rarely and carefully.
5. **A genuinely different technology need** — and not just "the team wants to try Go".

Not good reasons on their own: "microservices are modern", "the code is messy" (a split codebase is
messier), "it will scale better" (a monolith scales horizontally fine — see
[load balancing](/posts/load-balancing-and-stateless-servers)).

## A sensible path

```text
 1. Monolith with clean modules  ->  2. Enforce module boundaries (own data, public interfaces)
                                       |
           measured pain from one of the forces above
                                       v
 3. Extract ONE module whose boundary is stable  ->  4. Communicate via async events where possible
                                       |
 5. Build platform basics before the next split: CI per service, tracing, service templates, on-call
```

Martin Fowler's "MonolithFirst" argument is that you rarely know the right boundaries at the start;
they emerge as the product matures. Wrong service boundaries are far more expensive to fix than wrong
module boundaries.

## Real stories worth reading

- **Segment** wrote "Goodbye Microservices" (2018) about consolidating more than a hundred destination
  services back into a single service after the operational overhead outweighed the benefits.
- **Amazon Prime Video**'s team described in 2023 how moving an audio/video monitoring tool from a
  distributed serverless design to a single process cut its infrastructure cost dramatically. The
  lesson is not "microservices bad" — it's that the right granularity depends on the workload.
- **Uber** and **Netflix** went the other way at enormous scale, with large platform teams to support
  it.

## Key takeaways

- Monorepo/polyrepo is about *code storage*; monolith/microservices is about *runtime and deployment*.
- Microservices are an **organisational scaling tool** with a large technical cost.
- Start with a **well-modularised monolith**; extract services when a specific, measured force demands
  it.
- Avoid the distributed monolith: services must own their data and be deployable independently.

## Further reading

- Martin Fowler: [MonolithFirst](https://martinfowler.com/bliki/MonolithFirst.html) and [Microservice Premium](https://martinfowler.com/bliki/MicroservicePremium.html)
- Sam Newman, *Building Microservices* and *Monolith to Microservices* (books)
- Shopify Engineering: [Deconstructing the Monolith](https://shopify.engineering/deconstructing-monolith-designing-software-maximizes-developer-productivity)
- Segment: [Goodbye Microservices](https://segment.com/blog/goodbye-microservices/)
- [monorepo.tools](https://monorepo.tools/) — an overview of monorepo concepts and tooling
