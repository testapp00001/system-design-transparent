+++
title = "Distributed transactions: two-phase commit, sagas and the transactional outbox"
summary = "When one business operation spans several services or databases, a single ACID transaction is no longer available. How 2PC, sagas, the outbox pattern and change data capture keep data consistent anyway."
tags = ["distributed-systems", "microservices", "database", "messaging"]
level = "advanced"
date = 2026-10-02
+++

Placing an order means: create the order, reserve stock, charge the card, schedule shipping. In a
monolith with one database, that's one transaction — all or nothing. Split it across an order service,
an inventory service and a payment provider, each with its own data, and there is no single
transaction any more. What happens if the card is charged but the stock reservation fails?

This article covers the main tools for keeping data consistent across boundaries.

## The dual-write problem (the smallest version of the problem)

Even within one service, writing to two systems is unsafe:

```python
db.insert(order)                 # 1. commits
broker.publish("OrderPlaced")    # 2. process crashes before this -> other services never hear about it
```

There's no ordering of these two lines that is safe. Any time you write to a database **and** a message
broker, cache, search index or external API "at the same time", one can succeed while the other fails.
Keep this problem in mind — every solution below is a way around it.

## Two-phase commit (2PC)

A **coordinator** asks every participant to *prepare* (do the work, hold locks, promise to commit),
and only if all say yes, tells them all to *commit*.

```text
coordinator          participant A          participant B
    | -- prepare -->      |                      |
    | -- prepare ---------------------------->   |
    | <-- yes ----        |                      |
    | <-- yes --------------------------------   |
    | -- commit -->       |                      |
    | -- commit ----------------------------->   |
```

- **Gives** atomicity across resources (XA transactions; Postgres supports `PREPARE TRANSACTION`).
- **Costs:** participants hold locks while waiting for the coordinator; if the coordinator dies between
  phases, participants are stuck "in doubt" until it recovers (2PC is a *blocking* protocol); every
  transaction needs extra round trips; all participants must support the protocol — most HTTP APIs and
  message brokers don't.

2PC is used inside databases and some enterprise systems, but rarely across microservices. Modern
distributed databases (Spanner, CockroachDB) use consensus-replicated participants to make commit
protocols robust — inside one database system.

## Sagas: a sequence of local transactions with compensations

A **saga** breaks the operation into steps, each a local transaction in one service. If a step fails,
the saga runs **compensating actions** for the steps already completed, in reverse order.

```text
 1. Order:     create order (PENDING)            compensate: mark order CANCELLED
 2. Inventory: reserve stock                     compensate: release stock
 3. Payment:   charge card                       compensate: refund
 4. Order:     mark order CONFIRMED

 Step 3 fails -> release stock (2c) -> cancel order (1c)
```

Important properties:

- There is **no isolation**: other requests can see intermediate states (an order that is PENDING,
  stock that's reserved and later released). Design the UI and the data model for that ("processing
  your order…").
- Compensations are **semantic undo**, not rollback: a refund is not "the charge never happened" — the
  customer may see both on their statement. Some steps can't be compensated (an email was sent); put
  those last.
- Every step and compensation must be **idempotent and retryable**, because messages will be
  redelivered.

Two ways to coordinate a saga:

| | Choreography | Orchestration |
|---|---|---|
| How | Each service reacts to events and emits new ones | A central orchestrator tells each service what to do |
| Pros | No central component, loose coupling | The flow is explicit in one place; easy to see state, add steps, handle timeouts |
| Cons | The flow is implicit, spread across services; hard to follow beyond a few steps | The orchestrator is another service to build and run |
| Good for | Short, simple flows | Longer flows with many failure paths |

Workflow engines such as Temporal, AWS Step Functions or Camunda implement durable orchestration: the
orchestrator's state survives crashes, and it handles retries and timeouts for you.

## The transactional outbox

Sagas and event-driven systems depend on services reliably publishing events when their data changes
— which is the dual-write problem again. The **outbox pattern** solves it with your database's own
transaction:

```sql
BEGIN;
INSERT INTO orders (id, customer_id, status, total) VALUES (1042, 7, 'PENDING', 59.90);
INSERT INTO outbox (aggregate_id, event_type, payload)
VALUES (1042, 'OrderPlaced', '{"order_id":1042,"total":59.90}');
COMMIT;   -- both rows, or neither
```

A separate **relay** process reads unpublished outbox rows, publishes them to the broker, and marks
them as published:

```text
 service --(one transaction)--> [orders] + [outbox]
                                              |
                          relay: poll outbox --+--> broker --> other services
                                 mark published
```

- If the service crashes after commit, the event is still in the outbox and will be published.
- If the relay crashes after publishing but before marking, the event is published **again** — so
  consumers must be idempotent (at-least-once delivery, as always).
- Ordering per aggregate is preserved if the relay publishes in outbox order per key.

The mirror image on the consuming side is the **inbox**: record received message ids in the same
transaction as the side effects, so duplicates are ignored.

## Change data capture (CDC)

Instead of an application-level outbox table read by polling, you can stream the database's own
change log (PostgreSQL logical replication/WAL, MySQL binlog) into a broker. **Debezium** is the
best-known open-source tool. CDC can publish changes from tables directly, or efficiently relay an
outbox table (Debezium has an outbox event router for this).

- Pros: no polling, low latency, captures every change including those made outside the app.
- Cons: another component to operate; publishing raw table changes couples consumers to your schema
  (publishing outbox events avoids that).

## Choosing

```text
One database?                                      -> use a normal ACID transaction. Done.
Database + broker/cache/index in one service?      -> transactional outbox (or CDC)
Business process across services?                  -> saga (orchestrated if more than ~3 steps)
Strong atomicity across resources you control,
low volume, and participants that support XA?      -> 2PC may be acceptable
Need global transactions at scale?                 -> a distributed SQL database, not hand-rolled 2PC
```

And the most effective technique of all: **draw service boundaries so that most operations need only
one service's data**. If every operation spans three services, the boundaries are probably wrong (see
[monolith vs microservices](/posts/monolith-vs-microservices)).

## Further reading

- Chris Richardson, microservices.io: [Saga](https://microservices.io/patterns/data/saga.html) and [Transactional outbox](https://microservices.io/patterns/data/transactional-outbox.html)
- Hector Garcia-Molina & Kenneth Salem, *Sagas* (1987) — the original paper
- [Debezium documentation](https://debezium.io/documentation/) — including the outbox event router
- Pat Helland, *Life beyond Distributed Transactions: an Apostate's Opinion*
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapters 7 and 9
