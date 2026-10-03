+++
title = "Event sourcing and CQRS: storing what happened instead of what is"
summary = "Event sourcing stores every change as an immutable event and derives current state from them; CQRS separates writes from reads. How streams, projections, snapshots, schema versioning and crypto-shredding work, a PostgreSQL design, and when it is not worth it."
tags = ["architecture","distributed-systems","messaging"]
level = "advanced"
date = 2026-10-02
+++

A customer writes to support: "My balance says 40 euros. Yesterday it was 140. What happened?" You
open the `accounts` table. It has one row: `balance = 40`. That row tells you what **is**, not what
**happened**. Each `UPDATE` replaced the old value, so the story is gone. Now you search application
logs and hope someone logged the right thing.

**Event sourcing** turns this around. You store every change as a fact, in order, and never delete
it. The current state is calculated from those facts. This article explains how that works, why it
usually comes with **CQRS**, what it costs, and when a normal table is the better choice.

## The problem: a row only knows the present

In a typical CRUD (create, read, update, delete) application, each change overwrites data. That is
simple, but it loses information:

- "What was this balance on 3 March?" You cannot answer without a separate history.
- "Why is this order cancelled? Who did it?" Only if someone remembered to log it.
- "How many customers removed an item from the cart before paying?" The cart row only shows the end.

Teams often add audit tables later. These help, but they are a second copy of the truth, and the two
copies can drift apart without anyone noticing.

## How event sourcing works

A few terms first:

- A **domain event** is a fact that already happened in the business, named in the past tense:
  `AccountOpened`, `MoneyDeposited`, `OrderShipped`. Events are **immutable**: you never edit one.
- A **stream** is the ordered list of events for one thing, for example one bank account. Each event
  in a stream has a **version**: 1, 2, 3...
- An **aggregate** is the thing a stream describes and the unit that enforces business rules ("an
  account cannot go below zero"). As a rule, one command changes one aggregate. See
  [beyond MVC](/posts/beyond-mvc-project-architecture) for where such rules live in code.
- A **command** is a request to change something: `WithdrawMoney`. A command can be rejected. An
  event cannot, because it has already happened.

The event store is **append-only**: you only add events at the end of a stream.

```text
stream "account-42"   (append-only, ordered by version)

  v1  AccountOpened    {owner: "u-7"}
  v2  MoneyDeposited   {amount_cents: 10000}
  v3  MoneyWithdrawn   {amount_cents: 2500}
  v4  MoneyDeposited   {amount_cents: 500}
         |
         |  fold: start from an empty state, apply each event in order
         v
  current state:  balance = 8000 cents, version = 4
```

### Rebuilding state

To handle a command, you load the stream, **fold** the events into the current state (apply them
one by one), check the rules, and append new events:

```python
def apply(balance, event):
    if event.type == "MoneyDeposited":
        return balance + event.data["amount_cents"]
    if event.type == "MoneyWithdrawn":
        return balance - event.data["amount_cents"]
    return balance

def withdraw(stream_id, amount_cents):
    balance, version = 0, 0
    for e in load_events(stream_id):          # ORDER BY version
        balance, version = apply(balance, e), e.version
    if amount_cents > balance:
        raise InsufficientFunds()              # command rejected, nothing stored
    append(stream_id, expected_version=version,
           events=[Event("MoneyWithdrawn", {"amount_cents": amount_cents})])
```

Notice that `apply` has no rules and no side effects. It only says how a fact changes the state;
the rules live in the command handler. This matters because `apply` runs again every time the stream
is loaded, including years later.

### Snapshots

If a stream has thousands of events, loading it on every command becomes slow. A **snapshot** is
the saved state at some version ("balance 8000 at version 4000"). To load, you read the latest
snapshot and then only the events after it. Treat snapshots as a **cache**: they can be deleted and
rebuilt from events at any time, for example after the aggregate code changes.

Before adding snapshots, ask whether the stream should be that long. Many domains have natural
endings: one order, one claim, one month. "Closing the books" at the end of a period and starting a
new stream with the carried-over balance keeps streams short.

## A simple events table in PostgreSQL

You do not need a special database to start. One table is enough:

```sql
CREATE TABLE events (
    global_position BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    stream_id       TEXT        NOT NULL,   -- 'account-42'
    version         INT         NOT NULL,   -- 1, 2, 3... inside the stream
    event_type      TEXT        NOT NULL,   -- 'MoneyWithdrawn'
    schema_version  INT         NOT NULL DEFAULT 1,
    data            JSONB       NOT NULL,
    metadata        JSONB       NOT NULL DEFAULT '{}',  -- user id, correlation id...
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (stream_id, version)
);
```

The `UNIQUE (stream_id, version)` constraint does two jobs. It creates the index that makes "load one
stream, ordered by version" fast. And it gives you **optimistic concurrency**. Two requests load
`account-42` at version 7. Both decide to withdraw. Both try to insert version 8:

```sql
INSERT INTO events (stream_id, version, event_type, data)
VALUES ('account-42', 8, 'MoneyWithdrawn', '{"amount_cents": 2500}');
-- The second request fails with unique_violation (SQLSTATE 23505).
-- It reloads the stream (now at version 8), checks the rules again,
-- and either appends version 9 or rejects the command.
```

No locks are held while the business logic runs; the loser of a race simply retries. If one command
produces several events, insert them in one transaction. See
[transactions and isolation levels](/posts/transactions-and-isolation-levels) for why a constraint
is safer than a "SELECT max(version), then INSERT" check in code.

> [!WARNING]
> In PostgreSQL, identity and sequence values like `global_position` are handed out at insert time,
> not at commit time. Transaction A takes 1042, transaction B takes 1043 and commits first. A reader
> that saves "I am at 1043" will never see 1042. Some gaps also never fill: a transaction that rolls
> back still uses up its number. Common answers: wait a short, limited time for a gap to fill before
> moving past it (an event whose transaction commits even later is still missed, so keep append
> transactions short), or serialise appends (for example, every append first takes the same
> transaction-level advisory lock with `pg_advisory_xact_lock`; this limits write throughput).
> Dedicated event stores and libraries usually handle this for you.

## Projections and read models

The events table is great for loading one stream. It is terrible for "list all accounts with a
balance over 1,000 euros". For that you build **read models**: normal tables, search indexes or
caches shaped for one screen or query.

A **projection** (or projector) is code that reads events in order and updates a read model:

- `MoneyDeposited` → `UPDATE account_balances SET balance = balance + 10000 WHERE id = 42`
- `MoneyDeposited` → `INSERT INTO statement_lines (...)`

Each projector stores a **checkpoint**: the position of the last event it processed. If the read
model is in the same database, update the checkpoint in the same transaction, so a crash cannot apply
an event twice. If it lives elsewhere (a search index), make updates
[idempotent](/posts/retries-timeouts-and-idempotency) (safe to apply twice). For example, store the
last applied stream version in each read-model document and skip events that are not newer, or
write each entry under a key built from stream id and version, so a repeat overwrites the same entry.

The great feature: a read model is **disposable**. Need a report nobody planned for? Write a new
projection, run it from position 0, and it contains the full history. Found a bug in a projection?
Fix it, drop the table, replay.

## CQRS: separate models for writing and reading

**CQRS** (Command Query Responsibility Segregation) means using **different models** for changing
data and for reading it. It builds on Bertrand Meyer's "command-query separation" (CQS) principle: a
method should either change state or return data, not both. CQRS applies that split to whole models
instead of single methods. The name was popularised by Greg Young.

```text
 WRITE SIDE                                        READ SIDE
 command "WithdrawMoney"                           query "show my statement"
        |                                                   |
        v                                                   v
 +------------------+  append   +---------------+   +---------------------+
 | command handler  | --------> | event store   |   | read models         |
 | load stream,     |           | (append-only) |   |   account_balances  |
 | check rules,     | <-------- |               |   |   statement_lines   |
 | decide events    |   load    +-------+-------+   |   search index      |
 +------------------+                   |           +---------------------+
                                        |                      ^
                                        | events in order      | update
                                        +-----> projectors ----+
                                                (checkpoint = last position)
```

CQRS and event sourcing are separate ideas:

- **CQRS without event sourcing** is common: normal tables for writes, plus a denormalised table
  (data copied so reads need no joins), a materialised view or a search index for reads.
- **Event sourcing without CQRS** is rare in practice, because the event store cannot answer list or
  search queries efficiently.

### Read models are eventually consistent

When projectors run asynchronously, a read model is a little behind the event store: often less
than a second, but minutes or more during a replay or an incident. This is **eventual consistency**
(see [CAP and consistency models](/posts/cap-theorem-and-consistency-models)). The classic bug: the
user submits a form, is redirected to a list, and the new item is not there yet. Ways to handle it:

- Return the result (new state or version) from the command itself instead of reading it back.
- Return the `global_position` of the new event. The next query says "I need at least position X"
  and the server waits briefly until the projector has reached it.
- Update the few views that must be exact **inline**, in the same transaction as the append. This
  slows writes, so use it sparingly.

Measure **projection lag** (newest position minus checkpoint) and alert on it.

## Event sourcing is not event-driven architecture

These ideas are often mixed up, but they solve different problems:

| | Event sourcing | Event-driven architecture | Publishing events from a CRUD app |
|---|---|---|---|
| What events are for | The storage: the source of truth | Communication between services | Notifying others of a change |
| Where state lives | Derived from events | Each service decides | In normal tables |
| Who reads events | Mostly the owning service | Other services | Other services |
| Typical tools | Event store, events table | Kafka, RabbitMQ, SNS/SQS | [Outbox](/posts/distributed-transactions-saga-outbox), [CDC](/posts/change-data-capture) |

An event-sourced service can also be event-driven, but keep the two kinds of event separate. Internal
events change as your model changes; if other teams consume them, every refactoring breaks them.
Publish separate, stable **integration events**, for example from a projection that writes to an
outbox.

Can Kafka be the event store? It is excellent for distributing events (see
[queues and event streams](/posts/message-queues-and-event-streams)), but it has no built-in "append
only if this stream is still at version 7" check, and reading one aggregate's history usually means
scanning a partition shared with many other aggregates. Also, by default Kafka deletes old messages
after a retention period; you must configure topics to keep them forever.

## Events live forever: schema evolution

Rows can be migrated with `ALTER TABLE` and `UPDATE`. Events are immutable, and every replay reads
the old ones again. So you need a plan for changing their shape:

1. **Additive changes** are easiest: add an optional field, and use a default when an old event
   does not have it. Write readers so that they ignore fields they do not know.
2. **A new event type or schema version** for changes in meaning.
3. **Upcasting**: a function converts an old event to the newest shape *when it is read*, so the
   rest of the code only knows the latest version. Upcasters can be chained (v1 → v2 → v3).
4. **Copy and transform** the whole store into a new one: a last resort for big mistakes.

```python
from dataclasses import replace   # Event is assumed to be a dataclass
from decimal import Decimal

def upcast(event):
    # v1: {"amount": "25.00"}  (euros as text)
    # v2: {"amount_cents": 2500, "currency": "EUR"}
    if event.type == "MoneyWithdrawn" and event.schema_version == 1:
        cents = int(Decimal(event.data["amount"]) * 100)
        return replace(event, schema_version=2,
                       data={"amount_cents": cents, "currency": "EUR"})
    return event
```

Good event names help a lot. `CustomerAddressCorrected` and `CustomerMoved` mean different things to
the business. `CustomerUpdated` with a copy of the whole row means nothing, and it is hard to evolve.

## Deleting personal data: crypto-shredding

Laws such as the GDPR give people, in many situations, a right to have their personal data erased.
A log that "never forgets" conflicts with that. Common answers:

- **Keep personal data out of events.** Store names and emails in a normal table and put only an id
  in the event. Deleting the row is easy.
- **Crypto-shredding.** Encrypt personal fields with a key that belongs to one person, kept in a
  separate key store. To "forget" the person, delete the key. The encrypted bytes remain in the log
  and in backups, but nobody can read them any more, as long as the key is also gone from every
  backup of the key store.
- **Delete the stream**, if your event store supports it and the stream is about one person only.

> [!WARNING]
> Read models, caches, search indexes and exports may hold decrypted copies: delete them too. Losing
> the key store means losing data you wanted to keep. Whether shredding counts as "erasure" is a
> legal question for your data protection officer.

## Where it fits and where it hurts

| Good fit | Poor fit |
|---|---|
| Ledgers, payments, accounting: they are append-only by nature | Simple CRUD: settings, profiles, a content site |
| Audit-heavy domains: insurance, healthcare, compliance | Systems where most work is ad-hoc reporting over current data |
| Long workflows where "how did we get here?" matters: orders, claims, shipments | Teams under deadline pressure, new to the pattern |
| You expect new questions about the past | Apps where the history has no business value |

The costs are real: more moving parts (projectors, checkpoints, replays), and new habits. You cannot
"just fix the row"; you append a correcting event. Reporting needs projections or a data warehouse.
Apply event sourcing to **one part** of a system where the history is the business, not everywhere.

## Tools

| Tool | What it is |
|---|---|
| **KurrentDB** (formerly **EventStoreDB**) | A database built for event streams, with expected-version appends and subscriptions |
| **Marten** (.NET) | A library that uses PostgreSQL as a document database and event store, with inline and async projections |
| **Axon Framework** (Java) | A framework for aggregates, commands, queries and event sourcing, often used together with Axon Server from the same company |
| **Plain PostgreSQL** | The table above plus a projector loop. Enough for many teams, if you handle the ordering gaps |

## In practice: a checklist

- [ ] Event names are past-tense business facts, agreed with domain experts.
- [ ] Appends check the expected version (unique constraint or the store's own check).
- [ ] Streams are short; snapshots are added only after measuring.
- [ ] Every event has metadata: who, when, correlation id, causation id (what caused it).
- [ ] Every projection has a checkpoint, is idempotent, and can be rebuilt from zero. Test a full
      replay before you need one.
- [ ] Personal data has a deletion plan from day one.
- [ ] Tests read as "given these events, when this command, then these new events".

## Common mistakes

- **Event sourcing the whole system**, including parts that are plain CRUD.
- **CRUD events** like `OrderUpdated` carrying the whole row. You keep the cost and lose the meaning.
- **No concurrency check.** Two withdrawals both succeed and the balance goes negative.
- **Side effects during replay.** A projector that sends emails will send them again when you rebuild.
  Keep side effects in separate handlers that remember what they already did.
- **Letting other teams consume internal events**, which freezes your model.
- **Changing what an existing event means** instead of adding a new type or version.
- **Ignoring read-model lag** until users report "my change disappeared".

## Further reading

- Martin Fowler: [Event Sourcing](https://martinfowler.com/eaaDev/EventSourcing.html)
- Martin Fowler: [CQRS](https://martinfowler.com/bliki/CQRS.html)
- Martin Fowler: [What do you mean by "Event-Driven"?](https://martinfowler.com/articles/201701-event-driven.html)
- Microsoft Azure Architecture Center: [Event Sourcing pattern](https://learn.microsoft.com/en-us/azure/architecture/patterns/event-sourcing)
- [Marten documentation](https://martendb.io/) — event sourcing on PostgreSQL
- Greg Young: *Versioning in an Event Sourced System* (book) — schema evolution in depth
