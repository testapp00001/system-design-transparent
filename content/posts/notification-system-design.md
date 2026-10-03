+++
title = "Design a notification system: push, email, SMS and in-app"
summary = "How to send notifications that arrive once, on time and on the right channel: events, preferences and templates, per-channel queues, deduplication, quiet hours, retries and failover, device tokens, tracking and a real-time in-app inbox."
tags = ["system-design","realtime","messaging"]
level = "intermediate"
date = 2026-10-02
+++

Your app started with one line in the comment handler: `send_email(post.author, "New comment")`. Then
came push notifications, SMS login codes and a bell icon with unread items. A year later the
complaints arrive. Someone got the same push three times. A password-reset email arrived 40 minutes
late because a marketing campaign was sending at the same time. A user in Tokyo was woken at 3 a.m. by
"Your weekly summary is ready". The cause: there is no notification **system**, only notification calls
spread around the code. Let's design one, as in
[how to approach system design](/posts/how-to-approach-system-design): requirements and numbers
first, then architecture, then the hard parts.

**Words used here.** A **notification** is one message for one user ("Ana replied to your comment").
A **channel** is a way to reach the user: push, email, SMS or in-app (the list behind the bell icon).
A **provider** is the outside service that delivers on a channel. A **delivery** is one notification
on one channel. A **device token** is the address of one app installation on one device, issued by
Apple or Google.

## Requirements and estimates

What the system must do:

- Turn **events** from other services ("order shipped", "login code requested") into notifications:
  who, which channels, which text (from **templates** in the user's language).
- Respect user **preferences** ("comments: push yes, email no") and **quiet hours**.
- An **in-app inbox** with an unread count that updates in real time.
- Show what was sent, delivered and opened.

How well it must do it:

- **Two classes of traffic.** *Transactional* messages (login codes, password resets, security
  alerts) must arrive within seconds. *Bulk* messages (digests, campaigns) can wait, but must never
  slow down transactional ones.
- **No spam** (duplicates, floods), and **no silent loss**: a delay is acceptable, a drop is not.

Estimates for an app with 10 million daily active users:

| Channel | Assumption | Per day | Average rate |
|---|---|---|---|
| Push | 3 per active user | 30 million | ~350/s |
| Email | 1 per active user | 10 million | ~115/s |
| In-app | 5 per active user | 50 million | ~580 writes/s |
| SMS | Login codes only | 200,000 | ~2/s |

The averages are small. Two things are not. **Bursts:** a campaign to all 10 million users, sent
within 15 minutes, needs about 11,000 pushes per second. **Storage:** 50 million inbox rows per day at
about 500 bytes each (with indexes) is about 25 GB per day, or about 2.3 TB for 90 days. SMS volume is
tiny, but each message costs money.

## The architecture

```text
  product services ----- events -----> [ event queue ]
  (comments, orders,                          |
   auth, campaigns)                           v
                              +-------------------------------+
                              |      notification service     |
                              | 1. drop duplicates            |
                              | 2. load preferences, devices  |
                              | 3. choose channels            |
                              | 4. render templates (locale)  |
                              | 5. quiet hours, caps, batches |
                              | 6. save notification rows     |
                              +-------------------------------+
                                              |
          +-------------------+---------------+---+-------------------+
          v                   v                   v                   v
   [ push queue ]      [ email queue ]      [ sms queue ]     [ in-app queue ]
          |                   |                   |                   |
  [ push workers ]    [ email workers ]    [ sms workers ]    [ inbox writer ]
    |           |             |               |       |          |          |
  APNs         FCM     email provider       SMS A   SMS B    inbox DB    pub/sub
  (iOS)   (Android/web)       |                                             |
                      delivery webhooks                         [ SSE/WebSocket gateway ]
                         -> tracking                                        |
                                                                         the app
```

1. **Events in.** Services publish events through an outbox, so no event is lost between the database
   commit and the publish (see [distributed transactions](/posts/distributed-transactions-saga-outbox)
   and [message queues and event streams](/posts/message-queues-and-event-streams)). Producers say
   *what happened*, not *how to tell the user*.
2. **Per-channel queues**, so a slow SMS provider cannot delay emails. Give each channel a
   **high-priority** queue for transactional messages and a **bulk** queue, so a campaign cannot delay
   a login code.
3. **Workers** are stateless: take a delivery, call the provider, record the result.

### Channels and providers

| Channel | Provider | Address | Notes |
|---|---|---|---|
| iOS push | APNs (Apple Push Notification service), an HTTP/2 API | Device token | Payload up to 4 KB |
| Android push | FCM (Firebase Cloud Messaging) | Registration token | Needs Google Play services |
| Web push | FCM, or the standard Web Push protocol | Push subscription | User must allow it in the browser |
| Email | Amazon SES, SendGrid, Postmark, Mailgun... | Email address | Arrival depends on sender reputation |
| SMS | Twilio, Vonage, Amazon SNS... | Phone number | Paid per message; rules differ by country |
| In-app | Your database + SSE (Server-Sent Events) or WebSocket | User id | The only channel you fully control |

Two details that surprise people. One SMS holds 160 characters of the basic GSM-7 alphabet, but one
character outside it (an emoji, some accented letters) switches the message to UCS-2, which holds 70.
Longer texts are split into several paid parts, and each part holds a little less (153 or 67
characters), because a small header in each part tells the phone how to join them. And since 2024,
Gmail and Yahoo require bulk email senders to authenticate their mail with SPF, DKIM and DMARC (DNS
records and signatures that prove a message really comes from your domain), to keep spam complaints
low, and to support one-click unsubscribe (RFC 8058) in marketing mail.

### Preferences and templates

Store preferences as rows `(user_id, category, channel, enabled)`, with a few **categories** people
understand ("comments", "orders", "news and offers"). Security messages such as login codes ignore
preferences. A marketing opt-out always wins; in many countries the law requires consent, or at
least an easy unsubscribe, for marketing messages.

Key templates by `(type, channel, locale, version)`. Escape every user-provided value (a display name
can contain HTML). If a variable is missing, **fail** instead of sending "Hi {{name}}" to a million
people, and test-render every template in CI.

## Idempotency and deduplication

Duplicates come from three places:

1. **The event arrives twice.** Most queues deliver *at least once*.
2. **The product repeats itself.** The user likes, unlikes and likes again.
3. **The ambiguous send.** The provider accepts, but the worker crashes or times out before it
   records "sent". The retry sends again (see
   [retries, timeouts and idempotency](/posts/retries-timeouts-and-idempotency)).

```sql
CREATE TABLE notifications (
    id          BIGINT      PRIMARY KEY,
    user_id     BIGINT      NOT NULL,
    type        TEXT        NOT NULL,     -- 'comment.created', 'order.shipped', ...
    dedupe_key  TEXT        NOT NULL,     -- e.g. 'order.shipped:1042'
    payload     JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at     TIMESTAMPTZ,              -- for the in-app inbox
    UNIQUE (user_id, dedupe_key)
);

CREATE TABLE deliveries (
    notification_id BIGINT      NOT NULL REFERENCES notifications (id),
    channel         TEXT        NOT NULL, -- 'push' | 'email' | 'sms' | 'in_app'
    status          TEXT        NOT NULL, -- 'scheduled' | 'sending' | 'sent' | 'failed' ...
    send_after      TIMESTAMPTZ NOT NULL, -- quiet hours and batching move this
    expires_at      TIMESTAMPTZ,          -- after this, sending is pointless
    claimed_at      TIMESTAMPTZ,          -- when a worker moved it to 'sending'
    PRIMARY KEY (notification_id, channel)
);
```

- The **dedupe key** comes from the business event. Insert with `ON CONFLICT DO NOTHING`, and a
  repeated event does nothing.
- Workers **claim** a delivery by moving it from `scheduled` to `sending` in one conditional
  `UPDATE`. Only one worker wins. The same `UPDATE` sets `claimed_at`, so a delivery left in
  `sending` by a crashed worker can be picked up again after a timeout.
- The ambiguous send cannot be fully fixed, because many push, email and SMS APIs do not accept an
  idempotency key (check yours; some do). Keep the window small (record results immediately) and the
  damage small: set a collapse identifier (`apns-collapse-id` on APNs, the `tag` field for Android
  notifications in FCM) so a second copy replaces the first on screen.

After an ambiguous timeout, decide per type. Retry a login code (*at-least-once*): a duplicate is a
small annoyance, a missing code locks the user out. Don't retry a marketing push (*at-most-once*): a
duplicate looks like spam, a missing one costs little.

## Rate limits, batching and digests

- **Per-user caps**, such as "at most 3 marketing pushes per day" (a counter in Redis; see
  [rate limiting](/posts/rate-limiting)). Transactional messages are exempt.
- **Provider limits.** Amazon SES, for example, gives each account a maximum sending rate, and APNs
  returns `429` for too many notifications to one device token. Use a token bucket (a rate limiter
  that allows short bursts) per provider.
- **Your own capacity.** A push to 10 million users makes many of them open the app within minutes.
  Spread big campaigns over time.

**Batching** turns 14 pushes into "Ana, Ben and 12 others liked your photo". The first like creates a
notification for `photo_555` with `send_after = now() + 10 minutes`. Each new like looks for a
notification for the same photo that is still `scheduled`: if there is one, it updates it (add a
name, increase the count); if not, it starts a new batch. So give each batch its own dedupe key (for
example `likes:photo_555:` plus the id of its first like). A fixed key like `likes:photo_555` would
block every batch after the first. Batching adds delay, so never batch transactional messages.
**Digests** are daily or weekly emails built from unread inbox items; skip what the user has
already read.

## Quiet hours and time zones

- Store the user's **IANA time zone name** (a name from the standard time zone database, such as
  `Europe/Berlin`), not an offset like `+01:00`. Offsets change with daylight saving time; names
  don't.
- During quiet hours, move `send_after` of non-urgent deliveries to the end of the quiet period. Login
  codes and "your driver is here" ignore quiet hours.
- Time zones range from UTC−12 to UTC+14 (26 hours apart), so a "9:00 local time" campaign runs for
  more than a day.
- A scheduler picks due rows with `FOR UPDATE SKIP LOCKED`, as in
  [background jobs](/posts/background-jobs-and-cron).

```python
from datetime import datetime, time, timedelta, timezone
from zoneinfo import ZoneInfo

def next_allowed_send(now_utc, tz_name, quiet_start=time(22), quiet_end=time(8)):
    # quiet hours cross midnight (22:00-08:00); now_utc must be timezone-aware
    tz = ZoneInfo(tz_name)
    local = now_utc.astimezone(tz)
    if quiet_end <= local.time() < quiet_start:
        return now_utc                                    # not quiet: send now
    day = local.date() if local.time() < quiet_end else local.date() + timedelta(days=1)
    return datetime.combine(day, quiet_end, tzinfo=tz).astimezone(timezone.utc)
```

## Retries, expiry and provider failover

| Result | Examples | Action |
|---|---|---|
| Success | APNs `200`; a message id from an email or SMS API | Mark `sent` |
| Permanent failure | APNs `410 Unregistered`, `400 BadDeviceToken`; FCM `UNREGISTERED`; invalid number | Don't retry; fix the data |
| Transient failure | Timeouts, `5xx`, `429` | Retry later with backoff and jitter; respect `Retry-After` if the provider sends it |
| Too late | `expires_at` has passed | Drop it, record `expired` |

A late notification can be worse than none ("your driver is arriving", 40 minutes later). Give each
type a time to live and pass it to the provider (APNs `apns-expiration` header, `ttl` in FCM's
Android settings). Apple describes this as best effort, so a message can still arrive after that time.
After the last attempt, move the delivery to a dead-letter queue (a queue for messages that failed
for good, kept for inspection) and alert.

**Failover** works for email and SMS: integrate a second provider and switch when a
[circuit breaker](/posts/resilience-patterns) sees the first one failing. Send a small share of real
traffic through the backup all the time, so you know it still works. Push has **no failover**: APNs
is the only way to reach an iOS app (FCM and other push services also deliver to iOS through APNs).
If it is down, queue, wait and respect expiry. Falling back to another *channel* ("send the login
code by SMS") is a product decision.

## Device token lifecycle

Keep `device_tokens (token, user_id, platform, environment, last_seen_at)`.

- **Register on every app start.** Tokens can change; upsert the current one with
  `last_seen_at = now()`.
- **Detach on logout.** Otherwise the next person who logs in on that shared tablet gets the previous
  user's messages.
- **Remove dead tokens**: APNs `410` (`Unregistered`, `ExpiredToken`), `400 BadDeviceToken`, FCM
  `UNREGISTERED`. Apple's documentation says not to retry these. `BadDeviceToken` also appears when
  the token does not match the server's environment (for example, a development token sent to the
  production APNs server). So store the environment, and check it before you delete a token.
- **Expire stale tokens.** Firebase's token management guide recommends storing a timestamp with each
  token and removing tokens that have not been refreshed for a long time.

## Delivery and open tracking

"Sent" means **the provider accepted the request**, not that the user saw anything.

- **Push:** APNs only tells you whether it accepted the request. Put the notification id in the
  payload and let the app report taps to your API, which also marks the inbox item as read.
- **Email:** providers post events (delivered, bounced, complained, opened, clicked) to your
  [webhook](/posts/webhooks-reliable-delivery). Hard bounces and spam complaints must add the address
  to a **suppression list**. Opens (measured with a tiny image) are unreliable: Apple's Mail Privacy
  Protection loads remote images in the background, even for messages the user never opens, so
  "opens" appear that never happened. Trust clicks more.
- **SMS:** providers can report delivery receipts to a webhook; their quality varies by country.

Also count **opt-outs** per type. A type with many opt-outs is a product problem.

## The in-app inbox and real-time delivery

The inbox is the `notifications` table: list it with
[cursor pagination](/posts/api-design-pagination-versioning) over an index on `(user_id, id DESC)`,
set `read_at` when items are read, and keep an unread count (a partial index on unread rows, or a
cached counter).

For real time, the inbox writer saves the row, then publishes to the user's channel in a
[pub/sub system](/posts/pub-sub-redis-nats); the gateway holding the user's connection forwards it.
Data flows only from server to client, so [SSE](/posts/realtime-polling-sse-websockets) is usually
enough:

```text
id: 81723
event: notification
data: {"id": 81723, "title": "Ana replied to your comment", "unread": 4}
```

As in [scaling WebSockets](/posts/scaling-websockets-chat), **the database is the source of truth and
the socket is only a hint.** If a message is lost, the browser reconnects with `Last-Event-ID` and the
client fetches what it missed. If the user is active in the app right now
([presence](/posts/presence-at-scale)), skip the push and only update the inbox.

## Scaling and failure modes

Services and workers are stateless, so you add instances. The inbox grows fastest: partition it by
time, drop old partitions, and [shard by user id](/posts/sharding-and-partitioning) when one database
is not enough. One catch: in PostgreSQL, a unique constraint on a partitioned table must include the
partition column. So you cannot keep `UNIQUE (user_id, dedupe_key)` on a table partitioned by
time. Keep the dedupe keys in a separate, smaller table instead, and delete old keys after a few days.

| Failure | What users see | Defence |
|---|---|---|
| Campaign fills the queues | Login codes arrive late | Separate priority queues and workers |
| Provider outage | Nothing on one channel | Backoff, expiry, failover for email/SMS |
| Producer bug loops | The same push 50 times | Dedupe keys, per-user caps, kill switch |
| Everyone opens the app at once | Your API overloads | Spread big sends over time |

Give every notification type a **kill switch** (a [feature flag](/posts/feature-flags)). Watch queue
depth, the **age of the oldest message** per queue, errors per provider and error code, and the time
from event to "sent" (see [observability](/posts/observability-logs-metrics-traces)).

## When not to build all of this

A small product needs a background job, one email provider and an inbox table; add the rest when a
real problem appears. Hosted services (Amazon SNS or OneSignal for push; Novu, Knock or Courier for
more) cover parts of it, but you still decide what deserves a notification at all.

## Common mistakes

- Calling providers inside the request handler: a slow provider makes your API slow.
- One queue for everything: a campaign delays login codes.
- Treating "sent" as "delivered", and email opens as reads.
- Keeping dead tokens; storing UTC offsets instead of time zone names.
- Retrying permanent errors; ignoring bounces and complaints.

## Further reading

- Apple: [Sending notification requests to APNs](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns)
- Apple: [Handling notification responses from APNs](https://developer.apple.com/documentation/usernotifications/handling-notification-responses-from-apns)
- Firebase: [Best practices for FCM registration token management](https://firebase.google.com/docs/cloud-messaging/manage-tokens)
- RFC 8058: [Signaling One-Click Functionality for List Email Headers](https://www.rfc-editor.org/rfc/rfc8058)
- RFC 8030: [Generic Event Delivery Using HTTP Push](https://www.rfc-editor.org/rfc/rfc8030)
