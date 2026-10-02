+++
title = "Webhooks: sending and receiving events reliably"
summary = "How to send webhooks that survive customer outages (outbox, workers, retries, signatures, SSRF protection) and how to receive them safely (verify, acknowledge fast, deduplicate, handle events out of order)."
tags = ["api-design","reliability","messaging"]
level = "intermediate"
date = 2026-10-02
+++

Your SaaS product sends invoices. A customer asks: "Can you call our server when an invoice is paid?"
You add one line to the payment handler: `requests.post(customer_url, json=event)`. It works in the
demo. Then the customer's server is down for two hours and every event in that window is lost. Another
customer's endpoint takes 30 seconds to answer, and your payment API becomes slow. Someone registers
`http://169.254.169.254/` as their URL and tries to use your delivery log to read your cloud metadata.

A **webhook** is an HTTP request that your server sends to a URL chosen by someone else, to tell them
that something happened. Doing it reliably and safely is a small distributed system. This article
covers both sides: **sending** (you are the provider) and **receiving** (you integrate with Stripe,
GitHub, a shipping company and so on).

## Words used in this article

- **Event**: something that happened, such as `invoice.paid`. It has a unique id.
- **Endpoint**: a URL that a customer registered to receive events, plus a secret for signatures.
- **Delivery**: one event going to one endpoint. One event with three endpoints means three deliveries.
- **Attempt**: one HTTP request for a delivery. A delivery may need many attempts.

## Why the naive version fails

| Naive approach | What goes wrong |
|---|---|
| Send the HTTP request inside the API handler | A slow customer endpoint makes *your* API slow |
| Send it right after the database commit | A crash between commit and send loses the event |
| No retries | Any customer outage or deploy loses events forever |
| No signature | Anyone who knows the URL can send fake "invoice paid" events |
| Call any URL the customer types | Your server becomes a tool for attacking your own network |

The second row is the **dual-write problem**: you cannot write to your database and send a network request
atomically. The fix is an outbox, as in [distributed transactions](/posts/distributed-transactions-saga-outbox).

## The sending side: how it works

```text
  [API handler]
       |  one transaction: UPDATE invoices + INSERT INTO webhook_events   (the outbox)
       v
  [dispatcher]          creates one webhook_deliveries row per subscribed endpoint
       |
       v
  [webhook_deliveries]  status, attempts, next_attempt_at
       |
       v
  [delivery workers] ---- POST, 10 s timeout ----> customer URL
       |
       +-- 2xx                -> mark succeeded
       +-- error or timeout   -> attempts + 1, retry later with backoff
       +-- failing for days   -> disable the endpoint, email the owner
```

### 1. Record the event in the same transaction

```sql
BEGIN;
UPDATE invoices SET status = 'paid' WHERE id = 981;
INSERT INTO webhook_events (id, account_id, type, payload)
VALUES ('evt_7f3a9c', 42, 'invoice.paid', '{"invoice_id": 981, "amount": 2000}');
COMMIT;  -- both rows or neither
```

If the process crashes after the commit, the event is safe in the table. A **dispatcher** then
creates one row in `webhook_deliveries` per endpoint subscribed to `invoice.paid`, with a
`UNIQUE (event_id, endpoint_id)` constraint so that running the dispatcher twice creates no duplicates.

### 2. Deliver from workers, with short timeouts

**Workers** are background processes that pick due deliveries (`next_attempt_at <= now()`) and send
them. This is a normal [job queue](/posts/background-jobs-and-cron); a PostgreSQL table with
`FOR UPDATE SKIP LOCKED` or a message broker both work.

Use a **short timeout** for the whole request, such as 10 to 30 seconds (the Standard Webhooks
specification, described below, suggests 15 to 30 seconds). Every second a worker waits for someone
else's server is a second it cannot deliver to other customers. Document the limit.

Count only `2xx` responses as success. Everything else is a failure: `4xx`, `5xx`, timeouts,
connection errors, and also redirects (see the SSRF section).

### 3. Retry with exponential backoff, for hours or days

Customer outages can be long: a broken deploy, an expired TLS certificate, a server down over a
weekend. Retrying for one minute is not enough. Use **exponential backoff** (each wait is longer
than the last) with **jitter** (a random extra delay, so retries do not arrive in waves). An example
schedule (an illustration, not any provider's real schedule; jitter makes the times approximate):

| Attempt | Wait before this attempt | Time since the event |
|---|---|---|
| 1 | none | 0 |
| 2 | 1 minute | 1 minute |
| 3 | 5 minutes | 6 minutes |
| 4 | 30 minutes | 36 minutes |
| 5 | 2 hours | about 2.5 hours |
| 6 | 6 hours | about 8.5 hours |
| 7 | 12 hours | about 20.5 hours |
| 8 | 24 hours | about 2 days |
| 9 | 24 hours | about 3 days |

After the last attempt, mark the delivery `failed` and keep it visible. For comparison, Stripe's
documentation says that it retries live-mode deliveries for up to three days with exponential backoff.

### 4. Give every event a unique id

Delivery is **at least once**: the customer may process an event, the response is lost, and you
retry. Exactly-once *delivery* over a network is not possible; what receivers can achieve is
exactly-once *processing*, by dropping duplicates. So every event has a unique, stable id, and every
retry and resend uses the **same** id. Receivers use it to drop duplicates.

### 5. Do not promise ordering

Workers send in parallel, and retries change the order: event A fails, event B succeeds, A succeeds
an hour later. Strict ordering would mean one failing event blocks every later event for that
customer (**head-of-line blocking**). Many providers, Stripe among them, do not guarantee order.
Say so in your docs, and include a `created_at` timestamp in every event.

### 6. Keep one bad endpoint from hurting everyone

An endpoint that always takes the full 10 seconds to time out can occupy all your workers while
other customers wait. Limit the requests **in flight per endpoint**, and schedule fairly between
endpoints.

### 7. Disable endpoints that keep failing

Some endpoints are dead: the customer removed the integration but not the URL. After an
endpoint has failed every attempt for several days, **disable it automatically** and email the
account owner. This stops wasted work and tells a human who can fix it.

### 8. Build a replay UI and an events API

When a customer fixes their server, they need the events they missed. Give them:

- A **delivery log** per endpoint: each attempt with time, status code, latency and the start of
  the response body.
- A **resend** button, and "**replay all failed events since** a given time".
- An **events API** (`GET /events?after=<cursor>`) to catch up by code.

Replays reuse the original event id, so deduplicating receivers handle them safely.

## Signing: proving the event came from you

The receiver's URL is public, so anyone can send it a fake `invoice.paid`. To prevent that, each
endpoint gets a **secret** that only you and the customer know. For each attempt you compute an
**HMAC** (hash-based message authentication code, RFC 2104): a hash of the message mixed with the
secret. Without the secret nobody can produce a valid HMAC, and changing one byte of the body breaks it.

Sign the **event id and a timestamp together with the body**. The timestamp lets the receiver reject
old messages, so an attacker who records a valid request cannot **replay** it (send it again) next
week. The attacker cannot change the timestamp, because it is inside the signature.

The example below follows the [Standard Webhooks](https://www.standardwebhooks.com/) specification,
an open set of guidelines that aims to give providers one common format. It uses three headers:
`webhook-id` (the event id, the same on every retry), `webhook-timestamp` (Unix time in seconds) and
`webhook-signature`. The signed content is `<id>.<timestamp>.<body>`.

```python
import base64, hashlib, hmac, time

class InvalidSignature(Exception):
    pass

def key_from_secret(secret: str) -> bytes:
    # Standard Webhooks secrets look like "whsec_<base64>"; the HMAC key is the decoded bytes
    return base64.b64decode(secret.removeprefix("whsec_"))

def sign(key: bytes, event_id: str, timestamp: int, body: bytes) -> str:
    signed_content = f"{event_id}.{timestamp}.".encode() + body
    mac = hmac.new(key, signed_content, hashlib.sha256).digest()
    return "v1," + base64.b64encode(mac).decode()

def verify(key: bytes, headers, raw_body: bytes, tolerance_s: int = 300) -> None:
    try:
        event_id = headers["webhook-id"]
        timestamp = int(headers["webhook-timestamp"])
        candidates = headers["webhook-signature"].split()
    except (KeyError, ValueError):
        raise InvalidSignature("missing or malformed headers")
    if abs(time.time() - timestamp) > tolerance_s:
        raise InvalidSignature("timestamp outside tolerance")
    expected = sign(key, event_id, timestamp, raw_body).encode()
    # the header may hold several signatures (space-separated) during secret rotation
    for candidate in candidates:
        if hmac.compare_digest(candidate.encode(), expected):   # constant-time comparison
            return
    raise InvalidSignature("no matching signature")
```

Providers differ in the details. Stripe signs `<timestamp>.<body>` and sends
`t=<timestamp>,v1=<signature>` in a `Stripe-Signature` header. GitHub signs the body only (no
timestamp) and sends `sha256=<hex HMAC-SHA256>` in `X-Hub-Signature-256`. As a receiver, follow the
provider's documentation or use its official library.

> [!TIP]
> When a customer rotates their secret, keep the old one active for a while and **sign with both**.
> The receiver can switch over without losing events.

## SSRF: the customer's URL is untrusted input

**SSRF** (server-side request forgery) means an attacker makes your server send a request to a place
the attacker cannot reach directly: an internal admin service, a database with an HTTP interface, or
the cloud **metadata service** at `169.254.169.254`, which on many cloud setups can return access
credentials. A webhook system is an ideal SSRF tool, especially if your delivery log shows response
bodies.

Block destinations in non-public ranges, including:

| Range | What it is |
|---|---|
| `127.0.0.0/8`, `::1` | Loopback (the same machine) |
| `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | Private networks (RFC 1918) |
| `169.254.0.0/16`, `fe80::/10` | Link-local; the metadata address `169.254.169.254` is in this range |
| `100.64.0.0/10` | Shared address space (RFC 6598), meant for carrier-grade NAT; some internal networks and VPNs use it too |
| `fc00::/7` | IPv6 unique local addresses (AWS's IPv6 metadata address `fd00:ec2::254` is here) |
| `0.0.0.0/8` | "This network"; can reach the local machine on some systems |

**Check the IP address after DNS resolution, not the hostname.** `hooks.attacker.example` can resolve
to `10.0.0.5`. Worse, with **DNS rebinding** the name returns a public IP when you check it and a
private IP a moment later when the HTTP client connects. So resolve once, check every returned
address, and connect to exactly that address:

```python
import ipaddress, socket

class BlockedDestination(Exception):
    pass

def resolve_public_ips(host: str, port: int) -> list:
    infos = socket.getaddrinfo(host, port, proto=socket.IPPROTO_TCP)
    ips = []
    for info in infos:
        ip = ipaddress.ip_address(info[4][0])
        if ip.version == 6 and ip.ipv4_mapped:     # ::ffff:10.0.0.5 is really 10.0.0.5
            ip = ip.ipv4_mapped
        if not ip.is_global or ip.is_multicast:   # is_global covers all ranges above
            raise BlockedDestination(f"{host} resolves to {ip}")
        ips.append(ip)
    return ips  # connect to one of these; do not let the HTTP client resolve again
```

Also: do not follow redirects (or check every hop), require HTTPS where you can, and allow only the
standard ports (443, plus 80 only if you still accept plain HTTP). Many HTTP clients make "connect
to this exact IP" awkward, so a common design sends all webhook traffic through an **egress proxy**
(an outgoing proxy) that enforces these rules. Stripe has open-sourced the proxy it uses for this,
called Smokescreen, and the Standard Webhooks specification recommends this design. As a second
layer, run workers (or the proxy) in a network that cannot reach internal services.

## The receiving side

Now you are the customer. Four rules cover most problems.

### 1. Verify the signature on the raw body

Verify first, over the **raw request bytes**. If your framework parses the JSON and you serialise it
again, spacing or key order can change and the signature no longer matches. Compare with a
**constant-time** function (`hmac.compare_digest` in Python, `crypto.timingSafeEqual` in Node.js,
`hmac.Equal` in Go). A normal `==` usually stops at the first different byte, and that timing
difference can help an attacker guess a signature. If the provider signs a timestamp, reject old
ones (5 minutes is a common limit; Stripe's official libraries default to 300 seconds).

### 2. Acknowledge fast, process later

Store the event and return `200` at once. Do the real work (emails, order updates, API calls) in a
background job. If you process inside the request, a slow step causes a timeout, the sender retries,
and you process the same event twice.

```python
# Flask-style example. verify() is the function above, db is your database helper,
# WEBHOOK_KEY = key_from_secret("whsec_...") with the secret the provider gave you.
@app.post("/webhooks/payments")
def receive_webhook():
    raw = request.get_data()                    # raw bytes, before any JSON parsing
    try:
        verify(WEBHOOK_KEY, request.headers, raw)
    except InvalidSignature:
        return "", 400
    event = json.loads(raw)
    db.execute(
        "INSERT INTO webhook_inbox (event_id, type, payload) VALUES (%s, %s, %s) "
        "ON CONFLICT (event_id) DO NOTHING",    # duplicates are ignored here
        (request.headers["webhook-id"], event["type"], raw.decode()),
    )
    return "", 200                              # a worker processes webhook_inbox rows later
```

With Standard Webhooks the event id travels in the `webhook-id` header. Other providers put it in
the body (for example, Stripe events have an `id` field).

The `webhook_inbox` table is both your work queue and your record of received events.

### 3. Deduplicate by event id

You will receive the same event more than once. The `ON CONFLICT` above drops duplicates at the
door. If processing has side effects outside your database (an email, a card charge), make that step
idempotent too (see [retries and idempotency](/posts/retries-timeouts-and-idempotency)).

### 4. Expect events out of order

`subscription.updated` can arrive after `subscription.deleted`. If you apply each payload blindly, you
reactivate a cancelled subscription. Two safe approaches:

- **Treat the event as a hint and refetch.** Take the object id from the event and call the
  provider's API (`GET /subscriptions/sub_123`) for the **current** state. Order no longer matters.
  The cost is one API call per event, which counts against your [rate limits](/posts/rate-limiting).
- **Compare versions.** Store the object's `updated_at` or version number, and ignore any event older
  than what you already have.

Finally, do not trust webhooks to be complete. Endpoints get disabled, bugs drop events. A nightly
**reconciliation job** that compares your data with the provider's API catches what was missed.

## Trade-offs and when not to use webhooks

- **Between your own services**, use a [message queue or event stream](/posts/message-queues-and-event-streams).
  Webhooks are for crossing organisation boundaries, where you cannot share a broker.
- **For browsers and mobile apps**, use [polling, SSE or WebSockets](/posts/realtime-polling-sse-websockets).
- **Polling an events API** is a valid alternative for receivers: no public endpoint, no signatures,
  and the receiver controls the pace, at the cost of latency and load.
- **A full sender is real work**: outbox, queue, retries, signing, SSRF protection, a UI. Managed and
  open-source options exist (Svix is one); compare them with weeks of engineering and on-call time.

## Checklist

Sending:

- [ ] Events written in the same transaction as the change (outbox).
- [ ] Workers with a short timeout and a per-endpoint concurrency limit.
- [ ] Exponential backoff with jitter over hours or days.
- [ ] A stable unique event id, reused on retries and replays.
- [ ] Signatures over id, timestamp and body; rotatable secrets.
- [ ] IPs checked after DNS resolution; no redirects followed.
- [ ] Failing endpoints disabled automatically, owner notified.
- [ ] A delivery log and replay for customers.

Receiving:

- [ ] Signature verified on the raw body, constant-time compare.
- [ ] Store the event, return `2xx` within a few seconds.
- [ ] Duplicates dropped by event id.
- [ ] Out-of-order events handled by refetching or comparing versions.
- [ ] A reconciliation job catches missed events.

## Common mistakes

- **Generating a new event id on each retry**, which makes deduplication impossible.
- **Validating the URL only when it is saved.** DNS can change later; check at every connection.
- **Verifying a re-serialised body**, then disabling verification "because it never matches".
- **Logging webhook secrets** or putting them in error messages, where many people can read them.

## Further reading

- [Standard Webhooks](https://www.standardwebhooks.com/): an open webhook specification
- Stripe documentation: [Receive Stripe events in your webhook endpoint](https://docs.stripe.com/webhooks)
- GitHub Docs: [Validating webhook deliveries](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries)
- OWASP: [Server-Side Request Forgery Prevention Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html)
- [stripe/smokescreen](https://github.com/stripe/smokescreen): Stripe's outgoing HTTP proxy, which blocks internal IP addresses
- RFC 2104: [HMAC: Keyed-Hashing for Message Authentication](https://www.rfc-editor.org/rfc/rfc2104)
