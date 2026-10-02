+++
title = "Background jobs and cron: doing work outside the request, reliably"
summary = "What belongs in a background job, how job queues work (including a PostgreSQL queue with SKIP LOCKED), retries and idempotent jobs, and how to run scheduled jobs exactly once when you have many servers."
tags = ["messaging", "backend", "reliability", "postgresql"]
level = "intermediate"
date = 2026-10-02
+++

Some work doesn't belong inside an HTTP request: it's slow (generating a PDF report), it talks to
unreliable third parties (sending email), it must happen later (a reminder in 24 hours), or on a
schedule (nightly cleanup). Background jobs and scheduled tasks are how applications do that work —
and they come with their own failure modes.

## What should be a background job?

Move work out of the request when it is:

- **Slow**: anything that would make the user wait more than a few hundred milliseconds.
- **Unreliable**: calls to email/SMS providers, webhooks to customers, third-party APIs — they need
  retries with backoff, which you can't do while a user waits.
- **Deferred**: "send a reminder tomorrow at 9:00".
- **Bulk**: importing a CSV of 100,000 rows, re-indexing search, recalculating aggregates.
- **Periodic**: cleanup, reports, syncs, billing runs.

The request handler validates input, records the intent (e.g. "export requested"), enqueues a job and
returns immediately — often with a status page the user can poll.

## How a job queue works

```text
 web request --enqueue--> [ jobs table / queue ] <--poll/subscribe-- workers (N processes)
                                                                       |  run job
                                                                       |  success -> mark done
                                                                       |  failure -> retry later (backoff)
                                                                       |  too many failures -> dead jobs, alert
```

Popular libraries: Sidekiq (Ruby, Redis), Celery (Python), BullMQ (Node.js, Redis), Hangfire (.NET),
Laravel queues (PHP), Oban (Elixir, Postgres), River (Go, Postgres), Asynq (Go, Redis). You can also
use a message broker directly (see [queues and streams](/posts/message-queues-and-event-streams)).

## A job queue in PostgreSQL

If you already run PostgreSQL, you may not need Redis or a broker for jobs. Postgres has the key
feature: `FOR UPDATE SKIP LOCKED`, which lets many workers grab different jobs concurrently without
blocking each other.

```sql
CREATE TABLE jobs (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind        TEXT        NOT NULL,
    payload     JSONB       NOT NULL,
    run_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts    INT         NOT NULL DEFAULT 0,
    max_attempts INT        NOT NULL DEFAULT 10,
    locked_until TIMESTAMPTZ,
    last_error  TEXT,
    done_at     TIMESTAMPTZ
);
CREATE INDEX jobs_ready_idx ON jobs (run_at) WHERE done_at IS NULL;
```

A worker claims a job:

```sql
UPDATE jobs SET attempts = attempts + 1, locked_until = now() + interval '5 minutes'
WHERE id = (
    SELECT id FROM jobs
    WHERE done_at IS NULL
      AND run_at <= now()
      AND (locked_until IS NULL OR locked_until < now())   -- not claimed, or the claimer died
      AND attempts < max_attempts
    ORDER BY run_at
    LIMIT 1
    FOR UPDATE SKIP LOCKED                                  -- other workers skip rows we're locking
)
RETURNING id, kind, payload, attempts;
```

Then runs it, and either marks it done (`done_at = now()`) or schedules a retry with backoff
(`run_at = now() + interval '1 minute' * 2 ^ attempts`, `last_error = ...`).

The big advantage over an external queue: **enqueueing a job can be part of the same transaction as
your business data.** "Create the order and enqueue the confirmation email" either both happen or
neither does — no lost or phantom jobs. Postgres queues comfortably handle thousands of jobs per
second; beyond that, or for very large fan-out, a dedicated broker makes sense.

## Retries and idempotent jobs

Jobs fail: networks time out, deploys kill workers mid-job, third parties return errors. A job system
retries — which means **a job may run more than once**. Every job must be safe to repeat:

- "Send welcome email to user 42" → record `welcome_email_sent_at` and skip if set; or pass an
  idempotency key to the email provider.
- "Charge invoice 77" → use the payment provider's idempotency key derived from the invoice id.
- "Recalculate stats for day D" → compute and overwrite (idempotent by nature), don't increment.

Also:

- Use **exponential backoff with jitter** between attempts.
- Distinguish **retryable** errors (timeouts, 5xx) from **permanent** ones (invalid email address) —
  don't retry the latter 10 times.
- After the last attempt, keep the failed job visible (a "dead" state) and alert.
- Keep jobs **small**: pass ids, not big payloads, and load fresh data when the job runs. The data may
  have changed since enqueueing (the user may have deleted their account).

## Cron and scheduled jobs

The classic tool is `cron` on one server:

```cron
# m  h  dom mon dow  command
  0  3  *   *   *    /app/bin/cleanup-expired-sessions
```

Problems appear when you scale:

1. **Multiple instances run it N times.** Put the same cron in every container and the nightly billing
   run bills everyone three times.
2. **The one server running cron is a single point of failure** — and it's easy to forget it exists.
3. **Missed runs** during deploys or downtime are silently skipped.
4. **Overlapping runs**: a job scheduled every minute that sometimes takes 3 minutes overlaps with
   itself.
5. **Silent failure**: nobody notices the 3 a.m. job has been failing for weeks.

### Running scheduled work exactly once across many instances

Options, from simplest:

- **A dedicated scheduler**: Kubernetes `CronJob`, a cloud scheduler (EventBridge Scheduler, Cloud
  Scheduler), or one small "scheduler" process whose only job is to **enqueue** jobs into your job
  queue at the right times. Workers do the actual work.
- **A distributed lock**: every instance wakes up on schedule, but only the one that acquires a lock
  runs the job. In Postgres, advisory locks are ideal:

  ```sql
  BEGIN;
  SELECT pg_try_advisory_xact_lock(4242);  -- true for exactly one instance
  -- if true: do the work; the lock is released automatically at COMMIT/ROLLBACK
  COMMIT;
  ```

  This site does exactly this: every instance runs a loop every 60 seconds, and a `try` advisory lock
  ensures closing finished vote rounds happens once (see `src/worker.rs` and `finalize_ended_rounds`
  in `src/votes.rs`).

- **Make the job idempotent anyway.** "Close rounds that have ended and aren't closed yet" is safe to
  run twice; "close yesterday's rounds" is not. Prefer **"process whatever is due"** over
  **"process the period that just ended"** — it also catches up automatically after downtime.

### Watch your scheduled jobs

- Record each run (start, end, outcome, items processed).
- Use a **dead man's switch**: the job pings a monitoring service on success (e.g. Healthchecks.io, or
  your metrics system); you get alerted when the ping *doesn't* arrive.
- Prevent overlap with the same lock, or skip the run if the previous one is still going.

## Common mistakes

- Running long tasks in a fire-and-forget thread inside the web process — lost on every deploy or
  crash. (Short-lived tasks with a durable record are fine.)
- Enqueuing to an external queue *after* committing to the database (or before) — the dual-write
  problem; use the database queue or an outbox.
- Huge job payloads, or passing objects that become stale.
- No visibility: no dashboard of queue length, job age, failure rate.
- Time zones and DST: "every day at 02:30" doesn't exist on some days in some zones. Schedule in UTC
  unless the business rule is really about local time.

## Further reading

- PostgreSQL docs: [SELECT … FOR UPDATE SKIP LOCKED](https://www.postgresql.org/docs/current/sql-select.html#SQL-FOR-UPDATE-SHARE) and [Advisory Locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS)
- Kubernetes docs: [CronJob](https://kubernetes.io/docs/concepts/workloads/controllers/cron-jobs/)
- Sidekiq wiki: [Best Practices](https://github.com/sidekiq/sidekiq/wiki/Best-Practices) — idempotent, small jobs
