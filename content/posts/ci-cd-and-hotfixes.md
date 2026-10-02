+++
title = "CI/CD pipelines and the hotfix problem: 5 minutes by hand vs 30 minutes through the pipeline"
summary = "What CI/CD really is, what a good pipeline looks like, and how teams handle urgent fixes without editing production by hand: rollbacks, feature flags, fast pipelines and break-glass procedures."
tags = ["devops", "reliability", "backend"]
level = "intermediate"
date = 2026-10-02
+++

Production is broken. You know the fix: one line. SSH-ing into the server and editing the file would
take five minutes. Going through the pipeline — merge request, review, build, tests, staging, deploy
— takes thirty. Customers are affected *now*. What do you do?

If you have never worked with CI/CD, the honest answer seems obvious: edit the server. Teams that
run CI/CD answer differently, and the reason is not bureaucracy. This article explains what a pipeline
is for and the techniques teams use so that **the safe path is also the fast path** in an emergency.

## The vocabulary

- **Continuous integration (CI):** everyone merges small changes into the main branch frequently, and
  every change is automatically built and tested. Breakage is caught within minutes, while the change
  is small and fresh in someone's mind.
- **Continuous delivery:** every change that passes CI produces a deployable artifact, and deploying
  it is a push-button, automated, repeatable operation.
- **Continuous deployment:** every change that passes the pipeline is deployed to production
  automatically, with no human button at all.

The core idea of all three: **the path to production is code**, versioned, reviewed and identical
every time.

## What a pipeline typically does

```text
 push / merge request
   |
   v
 [lint + format] -> [build] -> [unit tests] -> [integration tests] -> [build image once, tag with commit SHA]
                                                                              |
                                      +---------------------------------------+
                                      v
                           [deploy to staging] -> [smoke tests] -> [deploy to prod: canary 5%]
                                                                          |
                                                    [watch error rate/latency] -> [100%] or [auto-rollback]
```

Key principles:

1. **Build once, promote the same artifact.** The Docker image tested in staging is byte-for-byte the
   one that goes to production. You never rebuild for prod.
2. **Every artifact is tagged with its commit** so you always know exactly what is running.
3. **Deploys are automated and repeatable** — the same script for every environment.
4. **Production changes are observable**: the pipeline checks health after deploying and can roll
   back automatically.

## Why editing production by hand is dangerous

The five-minute manual fix has hidden costs:

- **The next deploy erases it.** The pipeline deploys what is in Git. If the fix only exists on the
  server, the next routine deploy silently reintroduces the bug — often days later, when nobody
  remembers why.
- **Servers drift apart.** With three instances behind a load balancer, did you edit all three? The
  new container that the autoscaler starts tomorrow won't have it. You now have "snowflake servers"
  nobody can reproduce.
- **No review, no tests, under stress.** Emergency fixes are written by tired people in a hurry — the
  exact conditions in which a typo turns a partial outage into a full one.
- **No audit trail.** Later you cannot tell what changed, when, or why. Security and compliance teams
  care a lot about this.
- **Containers make it impossible anyway.** In Docker/Kubernetes, editing a running container is lost
  on restart. Immutable infrastructure forces the discipline.

So the goal is not "never be fast". It is **make the safe path fast**.

## Technique 1: roll back first, fix forward second

Most production incidents are caused by a recent change. The fastest fix is usually not a new commit
— it is **putting back the previous version**, which you already built and know works.

- Keep previous images/artifacts and make "deploy version N-1" a one-click (or one-command) action.
  In Kubernetes: `kubectl rollout undo deployment/api`. With images tagged by commit, any old version
  is one deploy away.
- A rollback takes a minute or two and needs no new build or tests: the artifact was already tested.
- Then write the real fix calmly, through the normal pipeline.

This only works if **database migrations are backward compatible** (the old code must run against the
new schema). That is why teams use the expand/contract approach described in
[deployment strategies](/posts/deployment-strategies-and-zero-downtime-migrations).

## Technique 2: feature flags and kill switches

A **feature flag** is a runtime switch, stored in a config service or database, that turns code paths
on or off without deploying:

```python
if flags.enabled("new-checkout", user):
    return new_checkout(cart)
return old_checkout(cart)
```

When the new checkout misbehaves, you flip the flag in seconds. No build, no deploy. Flags also let you
release features to 1% of users first, or to internal staff only. Build **kill switches** for risky
dependencies too ("disable recommendations if the recommender is down").

Flags are code and need hygiene: remove them once a feature is fully rolled out, or they accumulate
into an untestable maze of combinations.

## Technique 3: make the pipeline fast

A 30-minute pipeline is a problem for *every* change, not just emergencies — it batches changes up and
slows feedback. Common speedups:

| Problem | Fix |
|---|---|
| Downloading dependencies every run | Cache dependencies and build outputs (CI cache, Docker layer cache) |
| Rebuilding everything | Incremental builds; in monorepos, build/test only affected projects |
| Tests run one after another | Run jobs in parallel; split large test suites across several runners |
| Slow end-to-end suite on every commit | Keep a small critical-path smoke suite in the pipeline; run the full suite nightly or post-deploy |
| Rebuilding the image for each environment | Build once, promote |
| Waiting for a staging soak | Replace with automated canary analysis in production |
| Slow runners | Bigger machines are often cheaper than engineer time |

A common target is "commit to production in under 15 minutes". Many teams get well below that.

## Technique 4: an expedited (hotfix) lane

Some teams define a faster path for emergencies *inside* the pipeline rather than around it:

- Triggered explicitly (a `hotfix` label or branch), requires approval from an on-call lead.
- Runs the essential checks — build, unit tests, security scan — and skips the slow ones (full e2e,
  staging soak).
- Still produces a normal, tagged artifact and deploys through the normal deploy mechanism, so
  nothing drifts.
- The skipped checks run afterwards; failures open an incident.

The fix is reviewed (even a quick second pair of eyes catches a lot), versioned and reproducible, and
it takes minutes.

## Branching: where does the hotfix go?

- **Trunk-based development** (everyone merges to `main`, `main` is always deployable): fix on `main`,
  deploy `main`. Simplest; nothing to forget.
- **Release branches** (you deploy from `release/2.4`): fix on the release branch to deploy it, and
  immediately **merge or cherry-pick the same fix into `main`**. Forgetting this step is how the same
  bug "comes back" in the next release.

## Technique 5: break-glass access — for real emergencies only

Sometimes you truly must touch production directly: the pipeline itself is down, or data must be
repaired. Mature teams allow it with guard rails:

- Access is **temporary and audited** (time-limited credentials, every command logged).
- **Two people** are involved: one types, one watches.
- The change is **written down as it happens** and turned into code (a migration, a config commit)
  right after.
- A **postmortem** asks why the normal path was not enough, and fixes that.

## Measuring delivery: the DORA metrics

The DORA research program (now part of Google Cloud) found four metrics that distinguish high
performing teams:

- **Deployment frequency** — how often you deploy.
- **Lead time for changes** — commit to production.
- **Change failure rate** — share of deploys that cause a failure.
- **Time to restore** — how fast you recover when they do.

Their key finding: speed and stability are **not** a trade-off. Teams that deploy small changes often
also fail less and recover faster — because small changes are easy to review, test and roll back.

## A minimal pipeline to start with

This repository's own CI (`.github/workflows/ci.yml`) is a small example: format check, content
validation, lint, tests against a real Postgres service, and a Docker build. Starting points if your
company has nothing:

1. Run tests and lint on every pull request. Block merging when they fail.
2. Build a Docker image tagged with the commit SHA on every merge to `main`.
3. Automate deploying that image with one command; then trigger it from CI.
4. Add a health check and an automatic rollback.
5. Add feature flags for risky changes.

Each step pays off on its own. You don't need Kubernetes to do any of them.

## Further reading

- [DORA research](https://dora.dev/) — the four key metrics and the State of DevOps reports
- Martin Fowler: [Continuous Integration](https://martinfowler.com/articles/continuousIntegration.html)
  and [Feature Toggles](https://martinfowler.com/articles/feature-toggles.html)
- [trunkbaseddevelopment.com](https://trunkbaseddevelopment.com/)
- Jez Humble & David Farley, *Continuous Delivery* (book)
- Google SRE Book: [Release Engineering](https://sre.google/sre-book/release-engineering/)
