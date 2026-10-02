+++
title = "Message queues vs event streams: RabbitMQ, Kafka and delivery guarantees"
summary = "Why systems talk asynchronously, the difference between a queue and a log, at-most/at-least/exactly-once delivery, ordering, consumer groups, dead-letter queues and how to choose."
tags = ["messaging", "distributed-systems", "microservices", "backend"]
level = "intermediate"
date = 2026-10-02
+++

When a user signs up, your app must create the account, send a welcome email, add them to the CRM,
start a trial in billing and update analytics. Doing all of that inside the HTTP request makes signup
slow and fragile: if the CRM is down, signup fails. **Asynchronous messaging** lets the signup handler
record "user signed up" and return, while other components do their part in the background, at their
own pace, retrying on failure.

There are two main families of messaging systems, and they behave very differently.

## Why go asynchronous at all?

- **Decoupling in time:** the producer doesn't wait for consumers, and consumers can be down
  temporarily without losing work.
- **Load levelling:** a burst of 50,000 jobs is absorbed by the queue and processed at a steady rate.
- **Decoupling in space:** the producer doesn't need to know who consumes the message. New consumers
  can be added without touching the producer.
- **Fan-out:** one event can trigger many independent reactions.

The price: eventual consistency (the email arrives a few seconds later), harder debugging, and the need
to handle duplicates and ordering explicitly.

## Message queues (RabbitMQ, Amazon SQS, ActiveMQ, NATS JetStream work queues)

A **queue** holds messages until a consumer takes one, processes it and **acknowledges** it. Then the
message is gone. Multiple consumers on the same queue **compete** for messages, which spreads the work.

```text
 producers --> [ queue: m5 m4 m3 m2 m1 ] --> consumer A (got m1)
                                         --> consumer B (got m2)
                                         --> consumer C (got m3)
```

- Great for **tasks**: "send this email", "resize this image", "generate this invoice".
- Per-message acknowledgement, redelivery on failure, delays, priorities, TTLs.
- Routing: RabbitMQ exchanges can route by topic patterns or headers to many queues.
- Once consumed and acknowledged, a message can't be replayed.

## Event streams / logs (Apache Kafka, Redpanda, Amazon Kinesis, Apache Pulsar)

A **log** is an append-only, ordered sequence of events that is **kept** for a retention period (days,
or forever). Consumers don't remove events; each consumer group tracks its own **offset** (position)
in the log.

```text
 topic "orders", partition 0:   [0][1][2][3][4][5][6][7] <- producers append
                                          ^           ^
                       billing group offset=3     analytics group offset=7
```

- Multiple independent consumer groups read the same events, each at its own pace.
- **Replay:** reset an offset to reprocess history (fix a bug and recompute; build a new service from
  past events).
- **Partitions** provide parallelism: a topic is split into partitions, events with the same key (e.g.
  `order_id`) go to the same partition, and **order is guaranteed within a partition** only. Within a
  consumer group, each partition is read by one consumer at a time, so parallelism is capped by the
  number of partitions.
- Very high throughput (sequential disk writes, batching).
- Great for **events and data pipelines**: "order placed", change data capture, activity tracking,
  metrics, feeding search indexes and data warehouses.

## Queue vs log at a glance

| | Queue (RabbitMQ, SQS) | Log (Kafka, Kinesis) |
|---|---|---|
| Model | Work items, consumed once | Event history, read by many |
| After processing | Deleted | Retained, replayable |
| Multiple consumers | Compete for messages | Each group gets every event |
| Ordering | Usually best-effort (FIFO variants exist) | Strict per partition |
| Per-message retry/delay | Built in | You build it (retry topics) |
| Throughput | High | Very high |
| Typical use | Background jobs, task distribution | Event-driven integration, streaming, CDC |

You don't always need either: for background jobs in a single application, a **Postgres-backed
queue** is often enough — see [background jobs](/posts/background-jobs-and-cron).

## Delivery guarantees

- **At-most-once:** a message is delivered zero or one times. Acknowledge before processing — if the
  consumer crashes mid-way, the message is lost. Acceptable for metrics you can afford to drop.
- **At-least-once:** a message is delivered one or more times. Acknowledge *after* processing — if the
  consumer crashes after doing the work but before acknowledging, the message is redelivered and
  processed again. **This is the practical default.**
- **Exactly-once:** each message affects the system exactly once. True exactly-once *delivery* is
  impossible in general across independent systems; what systems offer is exactly-once *processing*
  within a boundary (e.g. Kafka transactions for consume-transform-produce within Kafka). Once your
  consumer writes to an external database or calls an API, you're back to at-least-once.

So the rule is: **assume at-least-once and make consumers idempotent.**

```sql
-- in the same transaction as the side effect:
INSERT INTO processed_messages (message_id) VALUES ($1)
ON CONFLICT DO NOTHING;
-- 0 rows inserted => already processed, skip the side effect
```

See [retries and idempotency](/posts/retries-timeouts-and-idempotency) for more techniques.

## Publishing reliably: the dual-write problem

A handler that does this has a bug:

```text
1. INSERT order into the database   (commits)
2. publish "order placed" to Kafka   (crashes before this line → event lost forever)
```

Reversing the order doesn't help (event published, insert fails → an event about an order that
doesn't exist). The fix is the **transactional outbox**: write the event to an `outbox` table in the
*same transaction* as the order, and have a separate relay publish rows from the outbox. Details in
[distributed transactions](/posts/distributed-transactions-saga-outbox).

## Ordering

Global ordering doesn't scale, so systems offer ordering per key:

- Kafka: same key → same partition → ordered. Choose keys by the entity whose events must be ordered
  (`order_id`, `account_id`).
- SQS FIFO queues: ordered per message group id.
- Retries can break ordering: if message 1 fails and is retried while message 2 succeeds, 2 is
  processed first. When order matters, either block the key until 1 succeeds, or make consumers
  tolerate out-of-order events (use version numbers and ignore older versions).

## Poison messages and dead-letter queues

A message that always fails (malformed payload, a bug) would be retried forever, blocking or wasting
consumers. After N attempts, move it to a **dead-letter queue (DLQ)**, alert someone, and continue.
Provide tooling to inspect DLQ messages and re-drive them after fixing the bug. A DLQ nobody watches
is just a slower way to lose data.

## Operational must-haves

- **Consumer lag** (how far consumers are behind) is the key metric. Alert when it grows.
- **Message schemas** with versioning (JSON Schema, Avro or Protobuf with a schema registry). Events are
  an API; breaking them breaks consumers you may not know about.
- **Correlation ids** in message headers so traces connect the HTTP request to the asynchronous work it
  caused.
- **Retention and size limits** set deliberately.

## Choosing

```text
"Run this task in the background, once"             -> queue (or a Postgres job table)
"Tell everyone who cares that X happened"            -> pub/sub topic or event log
"Several systems need the full history of changes"   -> event log (Kafka-like), CDC
"Very high throughput pipelines, replay, analytics"  -> event log
"Small app, one database, modest volume"             -> start with Postgres (SKIP LOCKED queue, LISTEN/NOTIFY)
```

## Further reading

- [RabbitMQ tutorials](https://www.rabbitmq.com/tutorials) — work queues, publish/subscribe, routing
- [Apache Kafka documentation: Design](https://kafka.apache.org/documentation/#design)
- Jay Kreps: [The Log: What every software engineer should know about real-time data's unifying abstraction](https://engineering.linkedin.com/distributed-systems/log-what-every-software-engineer-should-know-about-real-time-datas-unifying)
- AWS docs: [Amazon SQS dead-letter queues](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-dead-letter-queues.html)
