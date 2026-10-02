+++
title = "Beyond MVC: layered, hexagonal and clean architecture explained simply"
summary = "MVC organises a web request, not a business. How layered, hexagonal (ports and adapters) and clean architecture keep business logic independent of frameworks and databases — with a concrete folder structure and when it is overkill."
tags = ["architecture", "backend"]
level = "intermediate"
date = 2026-10-02
+++

Many developers learn one structure — **MVC** (model, view, controller) — and use it for everything.
It works well for small CRUD apps. Then the app grows: controllers reach 800 lines, business rules
are duplicated across controllers, models know about HTTP, and testing anything needs a database and
a web server. This article explains the architectures teams use beyond MVC, what problem each one
solves, and how to adopt them gradually.

## What MVC actually organises

MVC splits a **request** into: the controller (handle input), the model (data, usually ORM classes
mapped to tables) and the view (render output). It says nothing about **where business logic lives**.
In practice it ends up in one of two places:

- **Fat controllers** — the controller loads models, applies rules, calls the payment API, sends the
  email. The logic can't be reused from a CLI command or a background job without copying it.
- **Fat models** — business rules live in ORM classes, tying them to the database schema and making
  them impossible to test without a database.

The architectures below all answer one question: **how do we keep the business rules independent of
the delivery mechanism (HTTP, CLI, queue) and the infrastructure (database, external APIs)?**

## Layered architecture

The classic approach: split code into horizontal layers, each depending only on the one below.

```text
  presentation   (HTTP handlers, controllers, templates, CLI)
       |
  application    (use cases / services: "place order", "cancel subscription")
       |
  domain         (entities, value objects, business rules)
       |
  infrastructure (database access, external APIs, email, files)
```

- **Presentation** turns HTTP into a function call and the result back into HTTP. No business rules.
- **Application services** orchestrate one use case: load data, call domain logic, save, publish
  events. They define transaction boundaries.
- **Domain** contains the rules: "an order can't be cancelled after shipping", "a discount can't
  exceed the subtotal".
- **Infrastructure** implements technical details.

This is a big improvement over fat controllers. Its weakness: the domain sits *on top of*
infrastructure, so business code still tends to depend on database code.

## Hexagonal architecture (ports and adapters)

Proposed by Alistair Cockburn. The idea: put the **application core** (domain + use cases) in the
middle, and make **everything else a plug-in**.

- A **port** is an interface the core defines in its own terms: `OrderRepository`,
  `PaymentGateway`, `Notifier`.
- An **adapter** implements a port with a specific technology: `PostgresOrderRepository`,
  `StripePaymentGateway`, `SmtpNotifier` — or `InMemoryOrderRepository` for tests.
- **Driving adapters** call into the core (HTTP handler, CLI, queue consumer). **Driven adapters** are
  called by the core (database, payment API, email).

```text
         HTTP handler        CLI command        queue consumer        <- driving adapters
               \                  |                   /
                v                 v                  v
          +-----------------------------------------------+
          |   application core                            |
          |   use cases:  PlaceOrder, CancelOrder         |
          |   domain:     Order, Money, rules             |
          |   ports:      OrderRepository, PaymentGateway |
          +-----------------------------------------------+
                ^                  ^                  ^
               /                   |                   \
   PostgresOrderRepository   StripePaymentGateway   FakePaymentGateway   <- driven adapters
```

The key rule is the **dependency direction**: the core depends on nothing outside itself. Adapters
depend on the core (they implement its interfaces). This is the *dependency inversion principle*.

What you gain:

- **Fast tests of business logic** with in-memory adapters — no database, no network.
- **Swappable technology**: changing email provider means writing one adapter.
- **Multiple entry points** share the same use cases: the HTTP API, an admin CLI and a background job
  all call `PlaceOrder`.

## Clean architecture and onion architecture

Robert C. Martin's **Clean Architecture** and Jeffrey Palermo's **Onion Architecture** are close
relatives of hexagonal: concentric circles (entities → use cases → interface adapters → frameworks)
with one rule — **source code dependencies point inward**. If you understand ports and adapters,
you understand the essence of both; the differences are mostly vocabulary and how many rings they
draw.

## A concrete example

A use case written in hexagonal style (TypeScript, but the shape is the same in any language):

```ts
// core/ports.ts — the core defines what it needs
export interface OrderRepository {
  findById(id: string): Promise<Order | null>;
  save(order: Order): Promise<void>;
}
export interface PaymentGateway {
  refund(paymentId: string, amount: Money, idempotencyKey: string): Promise<void>;
}

// core/cancel-order.ts — business rules, no HTTP, no SQL
export class CancelOrder {
  constructor(private orders: OrderRepository, private payments: PaymentGateway) {}

  async execute(orderId: string, by: User): Promise<void> {
    const order = await this.orders.findById(orderId);
    if (!order) throw new NotFound("order");
    order.cancel(by);                       // throws if already shipped, or not the owner
    await this.payments.refund(order.paymentId, order.total, `refund-${order.id}`);
    await this.orders.save(order);
  }
}

// adapters/http/orders.ts — a thin driving adapter
router.post("/orders/:id/cancel", async (req, res) => {
  await cancelOrder.execute(req.params.id, req.user);
  res.status(204).end();
});
```

And a folder structure that makes the boundaries visible, organised by **feature first**:

```text
src/
  orders/
    domain/          Order, OrderStatus, rules (pure code)
    application/     PlaceOrder, CancelOrder (use cases), ports.ts
    adapters/
      http/          routes, request/response mapping
      postgres/      PostgresOrderRepository
  payments/
    ...
  shared/            Money, ids, clock, errors
  main.ts            composition root: wires adapters into use cases
```

Grouping by feature (`orders/`, `payments/`) rather than by technical type (`controllers/`,
`models/`, `services/`) keeps related code together, and each feature folder becomes a natural module
for a [modular monolith](/posts/monolith-vs-microservices).

## Vertical slices: a lighter alternative

**Vertical slice architecture** organises code around individual requests or use cases: each slice
contains its handler, validation, data access and response — minimal shared layers. It's pragmatic
for CRUD-heavy apps with little shared domain logic, and can be combined with a small shared domain
model where rules really are shared.

## When this is overkill

Ports, adapters and use-case classes have a cost: more files and indirection. Signs you don't need
them (yet):

- The app is mostly CRUD forms over tables with few rules.
- One small team, one entry point (HTTP), short expected lifetime.
- You'd be writing interfaces that will only ever have one implementation and no fakes in tests.

In those cases, a simple layered structure — thin handlers plus a service module per feature — is
enough. Introduce ports where the pain is: around **external services** (payments, email, third-party
APIs) and around logic you want to test fast. This site's own code uses that pragmatic middle ground:
thin HTTP handlers in `routes/`, domain modules with plain SQL (`posts.rs`, `votes.rs`), no
repository interfaces.

## Practical rules that work in any architecture

1. **Handlers are thin**: parse input, call one use case, map the result to a response.
2. **Business rules don't import web or database frameworks.**
3. **One place per decision**: if two endpoints need the same rule, it lives in one function.
4. **Side effects at the edges**: compute decisions in pure functions, then perform I/O.
5. **Transactions belong to the use case**, not to repositories or controllers.
6. **Wire dependencies in one place** (the composition root, `main`), not with global singletons.

## Further reading

- Alistair Cockburn: [Hexagonal Architecture](https://alistair.cockburn.us/hexagonal-architecture/)
- Robert C. Martin: [The Clean Architecture](https://blog.cleancoder.com/uncle-bob/2012/08/13/the-clean-architecture.html)
- Martin Fowler: [Presentation Domain Data Layering](https://martinfowler.com/bliki/PresentationDomainDataLayering.html)
- Jimmy Bogard: [Vertical Slice Architecture](https://www.jimmybogard.com/vertical-slice-architecture/)
- Eric Evans, *Domain-Driven Design* (book) — for modelling the domain itself
