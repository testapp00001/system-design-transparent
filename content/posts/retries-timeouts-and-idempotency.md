+++
title = "Retries, timeouts and idempotency keys"
summary = "Networks fail in ambiguous ways. Learn how to set timeouts, retry safely with backoff and jitter, and use idempotency keys so a retried payment is never charged twice."
tags = ["reliability", "api-design", "distributed-systems", "backend"]
level = "intermediate"
date = 2026-10-02
+++

A user taps **Pay**. Your server calls the payment provider. Ten seconds pass with no answer. Did the
charge go through? You don't know — and that uncertainty is the whole topic of this article.

In a single process, a function call either returns or throws. Over a network there is a third
outcome: **you don't know what happened**. The request may have been lost on the way, the server may
have crashed halfway, or it may have succeeded and the *response* was lost. Timeouts, retries and
idempotency are the three tools that, together, make systems behave correctly anyway.

## Timeouts: never wait forever

Every network call — HTTP, database, cache, queue — needs a timeout. Without one, a slow dependency
makes your threads/connections wait indefinitely, they pile up, and your whole service stops
responding. One slow downstream service becomes an outage everywhere upstream.

There are usually several timeouts:

- **Connect timeout** — how long to wait to establish the TCP/TLS connection. Short (hundreds of ms
  inside a data center): if you can't even connect, the server is likely down.
- **Request (read) timeout** — how long to wait for the response. Set it from the dependency's real
  latency: a bit above its p99 (99th-percentile latency), not "30 seconds because that's the default".
- **Overall deadline** — a budget for the whole user request. If the user-facing request has 2 seconds,
  a downstream call should not be allowed 5. gRPC propagates deadlines between services automatically;
  with HTTP you pass the remaining budget yourself (for example in a header).

> [!WARNING]
> Many HTTP clients have **no timeout by default**, or an extremely long one. Check yours — the
> default is a bug waiting for a bad day.

## Retries: when and how

Many failures are transient: a dropped connection, a server restarting during a deploy, a brief
overload. Retrying turns them into a little extra latency instead of an error. But careless retries
cause outages of their own.

### Only retry what can succeed next time

| Retry | Don't retry |
|---|---|
| Connection refused/reset, DNS hiccups | `400 Bad Request`, `422` validation errors |
| `502`, `503`, `504` | `401`, `403`, `404` |
| `429 Too Many Requests` (respect `Retry-After`) | Business errors ("insufficient funds") |
| Timeouts — **only if the operation is idempotent** (see below) | Anything non-idempotent without a key |

### Back off exponentially, with jitter

If 1,000 clients fail at the same moment and all retry after exactly 1 second, the recovering server
receives 1,000 requests at the same instant and falls over again — a **thundering herd**. Two fixes:

1. **Exponential backoff** — wait longer after each failure: 100 ms, 200 ms, 400 ms, 800 ms… capped at
   some maximum.
2. **Jitter** — randomise the wait so clients spread out. A widely used variant from AWS is "full
   jitter": sleep a random time between 0 and the exponential value.

```python
import random, time

def call_with_retries(fn, attempts=4, base=0.1, cap=2.0):
    for attempt in range(attempts):
        try:
            return fn()
        except TransientError:
            if attempt == attempts - 1:
                raise
            # full jitter: uniform in [0, min(cap, base * 2^attempt)]
            time.sleep(random.uniform(0, min(cap, base * 2 ** attempt)))
```

### Retries multiply across layers

Suppose a browser calls service A, which calls B, which calls C — and every layer retries 3 times.
When C is down, one user action becomes 3 × 3 × 3 = **27 requests** to C, exactly when C is least able
to handle them. Rules of thumb:

- Retry at **one layer**, usually the one closest to the failure or the edge — not everywhere.
- Use a **retry budget**: allow retries only while they are a small fraction (say 10%) of total
  traffic. When most calls are failing, retrying just adds load; fail fast instead (see
  [circuit breakers](/posts/resilience-patterns)).
- Keep the total retry time inside the caller's deadline.

## The hard part: retries and side effects

Back to the payment. The call timed out. If you retry and the first attempt *had* succeeded, the
customer is charged twice. If you don't retry and it had *failed*, the order is lost. You need the
operation to be safe to repeat. That property is **idempotency**:

> An operation is **idempotent** if performing it many times has the same effect as performing it once.

Some operations are naturally idempotent:

- `GET /orders/42` — reading changes nothing.
- `PUT /users/7/email` with a full value — setting X to 5 twice leaves it at 5.
- `DELETE /cart/items/3` — deleting twice leaves it deleted (the second call may return 404; the
  *state* is the same).

HTTP itself defines `GET`, `HEAD`, `PUT`, `DELETE` and `OPTIONS` as idempotent and `POST` as not.
Clients and proxies may automatically retry idempotent methods, which is one reason to honour these
semantics in your API.

Others are not: "charge $20", "add 1 to the counter", "append a message", "toggle the like". For
those, you add idempotency yourself.

## Idempotency keys

The pattern, popularised by Stripe's API: the **client** generates a unique key (a UUID) for each
*logical* operation and sends it with the request. Retries of the same operation reuse the same key.
The server remembers keys it has seen and, on a repeat, returns the **stored result** instead of doing
the work again.

```http
POST /v1/payments
Idempotency-Key: 8e03978e-40d5-43e8-bc93-6894a57f9324
Content-Type: application/json

{"order_id": 1042, "amount": 2000, "currency": "usd"}
```

```text
client                          server                               payments table / keys table
  | POST + key K  ------------->  | key K new? yes -> charge card --> | store (K, result)
  |      x  response lost         |                                   |
  | (timeout) retry POST + K ---> | key K seen, completed             |
  | <------------------ 200 + the original result, no second charge   |
```

### Implementing it with a database table

The crucial detail: recording the key and doing the work must be **atomic**. If you charge first and
crash before saving the key, the retry charges again. Use your database's guarantees:

```sql
CREATE TABLE idempotency_keys (
    user_id       BIGINT      NOT NULL,
    key           TEXT        NOT NULL,
    request_hash  TEXT        NOT NULL,          -- detect the same key with a different body
    status        TEXT        NOT NULL,          -- 'in_progress' | 'completed'
    response_code INT,
    response_body JSONB,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, key)
);
```

Handling a request:

1. `INSERT INTO idempotency_keys (user_id, key, request_hash, status) VALUES (..., 'in_progress')
   ON CONFLICT DO NOTHING`.
2. If the insert **succeeded**, this is the first attempt: do the work, then store the response and
   mark it `completed` — in the same transaction as your own business writes where possible.
3. If it **conflicted**, look at the existing row:
   - `completed` and same `request_hash` → return the stored response.
   - different `request_hash` → `422`: the client reused a key for a different request (a client bug).
   - `in_progress` → another attempt is running right now → `409 Conflict` (the client retries later).
4. Expire old keys (Stripe documents that keys can be pruned after 24 hours).

When the work includes a call to an external system (like the card network), you can't put it in your
database transaction. Then you also need the *external* call to be idempotent — which is exactly why
payment APIs accept idempotency keys themselves. Pass a key derived from yours along, and every hop is
safe to retry. Brandur Leach's article listed below shows how to structure this with "atomic phases"
and recovery points.

## Idempotency without keys

Often you can design the operation itself to be idempotent, which is simpler than key tables:

- **Unique constraints.** An `orders` table with `UNIQUE (client_order_id)` cannot get the same order
  twice: insert with `ON CONFLICT DO NOTHING` and return the existing row.
- **Set state instead of changing it.** "Set like = on" is idempotent; "toggle like" is not. This very
  site sends the desired state (`on=1`/`on=0`) when you like a post or vote, so a double click or a
  retry can't flip it back.
- **Conditional updates.** `UPDATE accounts SET balance = balance - 20, version = 8 WHERE id = 1 AND
  version = 7` only applies once (optimistic concurrency).
- **Deduplicate messages.** Queues usually deliver *at least once*, so consumers see duplicates.
  Record processed message ids in a table, in the same transaction as the effect, and skip ids you
  have seen.

## Checklist

- [ ] Every outbound call has a connect timeout and a request timeout, derived from real latency.
- [ ] Retries happen at one layer, with exponential backoff, jitter and a cap.
- [ ] Only transient errors are retried; `Retry-After` is respected.
- [ ] Non-idempotent operations exposed to retries (payments, orders, emails) accept an idempotency
      key or are made idempotent by design.
- [ ] Message consumers deduplicate.
- [ ] You can answer: "what happens if this request is sent twice?"

## Further reading

- AWS Architecture Blog: [Exponential Backoff And Jitter](https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/)
- Stripe: [Designing robust and predictable APIs with idempotency](https://stripe.com/blog/idempotency)
- Brandur Leach: [Implementing Stripe-like Idempotency Keys in Postgres](https://brandur.org/idempotency-keys)
- Google SRE Book: [Handling Overload](https://sre.google/sre-book/handling-overload/) and [Addressing Cascading Failures](https://sre.google/sre-book/addressing-cascading-failures/)
- RFC 9110, [section 9.2.2 Idempotent Methods](https://www.rfc-editor.org/rfc/rfc9110#section-9.2.2)
