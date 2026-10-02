+++
title = "Deployment strategies and zero-downtime database migrations"
summary = "Rolling, blue-green and canary deployments, feature flags, and the expand/contract technique that lets you change a database schema while old and new code both run."
tags = ["devops", "database", "reliability"]
level = "intermediate"
date = 2026-10-02
+++

"We deploy on Friday night when traffic is low, and the site is down for ten minutes" works until it
doesn't: users in other time zones, a migration that takes an hour instead of a minute, a bug found on
Monday that needs a rollback the database no longer supports. This article covers how teams deploy
many times a day without downtime — for both code and schema.

## The core constraint

During any non-instant deploy, **old and new versions of your code run at the same time**, against
**the same database**. Every technique below either manages that overlap or relies on it being safe.

## Deployment strategies

### Recreate (stop everything, start the new version)

Simple and guarantees no version overlap — at the cost of downtime. Fine for internal tools and
batch systems.

### Rolling update

Replace instances one (or a few) at a time: start a new instance, wait until it's healthy, route
traffic to it, drain and stop an old one, repeat. The default in Kubernetes Deployments and most
orchestrators.

- No extra capacity needed, no downtime.
- Old and new versions serve traffic simultaneously for the duration of the rollout.
- Rollback = another rolling update to the old version.

### Blue-green

Run two complete environments. "Blue" serves production; deploy the new version to "green", test it,
then **switch the router** to green in one step. Keep blue around for instant rollback.

- Instant switch and instant rollback.
- Double the capacity during the deploy.
- The database is usually shared by blue and green, so schema changes must still be compatible with
  both.

### Canary release

Send a small share of traffic (1%, then 5%, 25%, 100%) to the new version and **compare its metrics
with the old version** — error rate, latency, business metrics — before continuing. Stop and roll
back automatically if the canary is worse.

- Limits the blast radius of a bad release to a small fraction of users.
- Needs good observability and traffic-splitting (weighted load balancing, service mesh).
- Tools such as Argo Rollouts and Flagger automate the analysis.

### Feature flags: deploy ≠ release

Ship the code dark (disabled), then **release** the feature by turning on a flag — for internal users,
then 5% of customers, then everyone. Turning it off is the fastest rollback there is. Deployments
become routine and boring; releases become business decisions. See
[CI/CD and hotfixes](/posts/ci-cd-and-hotfixes).

| Strategy | Downtime | Extra capacity | Rollback speed | Version overlap |
|---|---|---|---|---|
| Recreate | Yes | No | Slow (redeploy) | No |
| Rolling | No | Little | Medium | Yes |
| Blue-green | No | 2× during deploy | Instant | Brief / none for traffic |
| Canary | No | Little | Fast | Yes, by design |
| Feature flag | No | No | Instant | Code paths, not versions |

## Zero-downtime database migrations

Code is easy to roll back; data is not. Schema changes are where most "zero-downtime" deploys
actually break. Two rules:

1. **Every migration must be compatible with the code version currently running *and* the one being
   deployed.**
2. **Migrations must not lock busy tables for long.**

### Expand / contract (parallel change)

Split every breaking change into backward-compatible steps, each its own deploy.

**Example: rename `users.name` to `users.full_name`.**

A naive `ALTER TABLE users RENAME COLUMN name TO full_name` breaks every running instance that still
selects `name`. Instead:

```text
 Deploy 1 (expand):   add column full_name (nullable).
                      Code writes BOTH name and full_name; still reads name.
 Backfill:            UPDATE users SET full_name = name WHERE full_name IS NULL — in batches.
 Deploy 2:            code reads full_name (falls back to name if null), still writes both.
 Deploy 3:            code reads and writes only full_name.
 Deploy 4 (contract): drop column name.
```

At every step, the previous code version still works, so each deploy can be rolled back. It feels
slow, but each step is small and safe — and with an automated pipeline, the four deploys may take an
afternoon.

The same pattern covers changing a column's type, splitting a table, moving data to a new table, or
making a column `NOT NULL` (add with a default/backfill → enforce once all writers set it).

### Avoid long locks

Schema changes take locks. On a busy table, even a brief exclusive lock **queues every other query
behind it** — and a migration waiting for a lock (because a long query is running) also blocks
everything queued after it. Practical rules for PostgreSQL (MySQL has similar concerns, with
`ALGORITHM=INSTANT/INPLACE` and tools like gh-ost or pt-online-schema-change):

- Set a **lock timeout** in migrations so they fail fast instead of blocking the site:
  `SET lock_timeout = '5s';` — and retry later.
- Create indexes **concurrently**: `CREATE INDEX CONCURRENTLY ...` (can't run inside a transaction).
- Adding a column with a constant default is fast in modern PostgreSQL (11+); adding one with a
  *volatile* default (like `random()`) rewrites the table.
- Add foreign keys and check constraints as `NOT VALID` first, then `VALIDATE CONSTRAINT` separately
  (validation doesn't block writes).
- Make a column `NOT NULL` safely by first adding a `CHECK (col IS NOT NULL) NOT VALID` constraint,
  validating it, then setting `NOT NULL` (PostgreSQL 12+ uses the validated check to skip the full
  scan).
- **Backfill in batches** (e.g. 1,000–10,000 rows per transaction, with short pauses), never one giant
  `UPDATE` that holds locks, bloats the table and floods replication.
- Changing a column's type usually rewrites the table — use expand/contract with a new column instead.

### Migrations and deploy order

- Run migrations as a separate pipeline step **before** rolling out new code (for expand steps), and
  contract steps only **after** no old code remains.
- Never put destructive changes (drop column/table) in the same deploy as the code that stops using
  them.
- Test migrations against a production-sized copy of the data: a migration that takes 50 ms on your
  laptop can take 40 minutes on a 200 GB table.

## A deploy checklist

- [ ] Can the previous version run against the new schema? (If not, split the migration.)
- [ ] Do migrations use lock timeouts and avoid table rewrites on large tables?
- [ ] Is there a health check, and will the rollout stop if it fails?
- [ ] Are error rate and latency watched during the rollout (ideally automatically)?
- [ ] Is rollback one command, and has it been tried recently?
- [ ] Is risky new behaviour behind a feature flag?

## Further reading

- Martin Fowler: [ParallelChange](https://martinfowler.com/bliki/ParallelChange.html), [BlueGreenDeployment](https://martinfowler.com/bliki/BlueGreenDeployment.html), [CanaryRelease](https://martinfowler.com/bliki/CanaryRelease.html)
- PostgreSQL docs: [ALTER TABLE](https://www.postgresql.org/docs/current/sql-altertable.html) (notes on locking and table rewrites) and [CREATE INDEX CONCURRENTLY](https://www.postgresql.org/docs/current/sql-createindex.html#SQL-CREATEINDEX-CONCURRENTLY)
- Kubernetes docs: [Performing a Rolling Update](https://kubernetes.io/docs/tutorials/kubernetes-basics/update/update-intro/)
- GitHub: [gh-ost](https://github.com/github/gh-ost) — online schema migrations for MySQL
