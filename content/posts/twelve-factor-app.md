+++
title = "The twelve-factor app, explained with modern examples"
summary = "Twelve-factor is a short list of rules for building services that any platform can run. Learn each factor with a concrete do and don't for Docker and Kubernetes, and where the list is now dated."
tags = ["devops","architecture","backend"]
level = "beginner"
date = 2026-10-02
+++

Your web app works on your laptop. You deploy it in a container, and strange things happen. The
database host is hard-coded to `localhost`. User uploads disappear when the container restarts. Logs
go to a file inside the container, so a crash takes the logs with it. You run three copies and users
are logged out at random. These are not bugs in your business logic. They are mismatches between how
the app was written and how platforms run software.

The **twelve-factor app** is a short list of rules that removes these mismatches. This article explains
each factor with a concrete "do" and "don't", and where the list is now dated.

## Where it comes from, and why it still matters

The twelve-factor app was published at [12factor.net](https://12factor.net/) around 2011 by engineers
at Heroku, an early **platform-as-a-service** (PaaS). A PaaS runs your code for you: you push code, and
it builds, starts and scales it. Adam Wiggins, a Heroku co-founder, is usually credited as the main
author. The team wrote down the patterns that made apps easy to run on their platform.

Docker and Kubernetes came later, but they expect the same things: read config from the environment,
listen on a port, write logs to standard output, keep nothing important on local disk, and exit
cleanly when asked. Twelve-factor describes the **contract between your app and the platform**:

```text
   the platform gives the app                           the app gives the platform
   --------------------------                           --------------------------
   config in environment variables  --->  +-----+  ---> logs as a stream on stdout
   a port to listen on              --->  |     |  ---> HTTP on that port
   addresses of backing services    --->  | app |  ---> fast start, clean exit
   SIGTERM before it stops the app  --->  +-----+  ---> no data it needs on local disk
```

Keep this contract and the app moves between a laptop, Docker Compose, a PaaS and Kubernetes with
little or no code change.

## The twelve factors, one by one

### 1. Codebase: one codebase, many deploys

One app has one codebase in version control (usually Git). A **deploy** is a running copy of the app:
production, staging, a developer's laptop. All deploys run the same codebase, maybe at different
commits.

- **Do:** build every environment from a commit, and tag the build with the commit ID, so you can
  always answer "what code is running?".
- **Don't:** keep a "production copy" with manual edits, or copy-paste code between two apps. Shared
  code becomes a library. (Monorepos fit the spirit if each app has its own build and deploy.)

### 2. Dependencies: declare and isolate them

Every dependency is listed explicitly, with exact versions. The app never relies on something that
"happens to be installed" on the server.

- **Do:** commit a lockfile (`package-lock.json`, `poetry.lock` or `uv.lock`, `Cargo.lock`; in Go,
  `go.mod` pins the versions and `go.sum` their checksums) and install from it in CI (`npm ci`, `cargo build --locked`). With containers, the Dockerfile
  declares system packages too.
- **Don't:** shell out to `curl` or ImageMagick because they exist on your laptop (the manifesto's own
  examples), or `pip install` things by hand on a production server.

### 3. Config: store it in the environment

**Config** is everything that changes between deploys: database URLs, credentials, hostnames, the
public URL of the site. It lives in environment variables, not in code. The manifesto gives a good
test: could you make the repository public right now without leaking any credentials?

In Kubernetes, the variables come from a ConfigMap (plain settings) and a Secret (credentials):

```yaml
env:
  - name: LOG_LEVEL
    valueFrom:
      configMapKeyRef: { name: api-config, key: log-level }
  - name: DATABASE_URL
    valueFrom:
      secretKeyRef: { name: api-secrets, key: database-url }
```

- **Do:** validate all config at startup, and stop with a clear message if something is missing.
- **Don't:** commit `.env` files, or scatter `if env == "production"` through the code. The manifesto
  prefers independent settings over named environments, because environments keep multiplying.

> [!WARNING]
> Environment variables are not a safe home for every secret. Child processes inherit them, and they
> can leak into crash reports or debug pages. Many teams mount secrets as files or read them from a
> secrets manager instead. See [secrets management](/posts/secrets-management).

### 4. Backing services: treat them as attached resources

A **backing service** is anything the app uses over the network: Postgres, Redis, a queue, an email
provider, object storage like S3. Local or managed by someone else, it is attached the same way: by a
URL and credentials from config.

- **Do:** use Postgres in Docker Compose locally and a managed Postgres in production, changing only
  `DATABASE_URL`.
- **Don't:** hard-code `localhost:5432`, or write `if production: upload to S3 else: write to disk`.

### 5. Build, release, run: keep the stages separate

```text
 commit 3f9c2e1 --build--> image app:3f9c2e1 --+
                                               +--release--> release v42 --run--> web    x3
 production config (env vars) -----------------+             (never edited)       worker x2
```

**Build** turns a commit into something runnable, today usually a container image. **Release**
combines that build with one deploy's config and gets a unique ID. **Run** starts processes from a
release. A release is never changed; to roll back, you run the previous one.

- **Do:** build the image once in CI, tag it with the commit ID, and promote the *same* image from
  staging to production.
- **Don't:** rebuild per environment, deploy the moving `latest` tag, or edit files on a live server.

See [CI/CD and hotfixes](/posts/ci-cd-and-hotfixes) and
[deployment strategies](/posts/deployment-strategies-and-zero-downtime-migrations).

### 6. Processes: stateless and share-nothing

The app runs as processes that keep **no data they need between requests**. Anything that must survive
goes to a backing service; local memory and disk are only for short-lived caches. The manifesto even
calls sticky sessions a violation: keeping a user's session in one process's memory and expecting
the load balancer to send that user back to the same process.

- **Do:** store sessions in the database or Redis (or in signed cookies), and uploads in object storage.
- **Don't:** save uploads to `./uploads` inside the container, or keep shopping carts in a global map.

See [load balancing and stateless servers](/posts/load-balancing-and-stateless-servers).

### 7. Port binding: the app brings its own server

The app contains its own HTTP server and **exports its service by listening on a port**, instead of
being deployed into a separate web server such as Tomcat or Apache. Heroku and Google Cloud Run, for
example, tell the app which port to use in a `PORT` environment variable.

- **Do:** listen on `0.0.0.0:$PORT` inside a container, with a reverse proxy or load balancer in front
  (see [reverse proxies and TLS](/posts/reverse-proxies-and-tls)).
- **Don't:** bind to `127.0.0.1` inside a container (Docker's published ports and other machines
  cannot reach it), or hard-code a port the platform cannot change.

### 8. Concurrency: scale out with processes

You scale by running **more processes**, split by **process type**: `web` for HTTP, `worker` for
background jobs, maybe `scheduler`. Each type scales on its own. Heroku describes them in a Procfile:

```text
web:    ./app serve
worker: ./app run-jobs
```

In Kubernetes, each process type usually becomes its own Deployment. Inside one process you can still
use threads or async code (see [concurrency models](/posts/concurrency-models)).

- **Do:** run jobs in separate worker processes, so a backlog does not slow down web requests. See
  [background jobs and cron](/posts/background-jobs-and-cron).
- **Don't:** daemonize, write PID files, or run your own supervisor inside a container. Restarting
  and scaling are the platform's job.

### 9. Disposability: fast start, graceful stop

Deploys, autoscaling and failing machines start and stop processes all the time. Processes should
start in seconds and stop cleanly.

```text
 t = 0      platform sends SIGTERM and starts removing the process from load balancing
            app stops taking new work, finishes in-flight requests,
            closes its connections (database, queue) and exits
 t = 30 s   still running? platform sends SIGKILL  (30 s = Kubernetes default grace period)
```

The wait before `SIGKILL` is configurable. In Kubernetes it is `terminationGracePeriodSeconds`;
`docker stop` waits 10 seconds by default.

- **Do:** handle `SIGTERM`, make jobs safe to run twice (a killed worker's job may be retried), and
  use the exec form `CMD ["./app", "serve"]` in Dockerfiles. The shell form (`CMD ./app serve`) starts
  your command through `/bin/sh -c`, and depending on the shell, `SIGTERM` may never reach your app.
- **Don't:** spend minutes warming caches at startup.

> [!NOTE]
> In a container, your app often runs as process ID 1 (PID 1). Linux ignores `SIGTERM` for PID 1
> unless the program installs a handler for it. So if the app does not handle the signal, nothing
> happens, and the app only stops at `SIGKILL`, after the whole grace period. Handle `SIGTERM` in code, or run a small init
> process in front of the app (for example `docker run --init`).

> [!TIP]
> Kubernetes removes a pod from load balancing at about the same time it sends `SIGTERM`, not strictly
> before, so a few new requests can still arrive. Keep serving for a few seconds after `SIGTERM`, or
> add a short `preStop` sleep. The `preStop` time counts toward the grace period.

### 10. Dev/prod parity: keep environments similar

The manifesto names three gaps: **time** (code waits weeks to be deployed), **personnel** (developers
write it, another team deploys it) and **tools** (different software locally). Continuous deployment
closes the first, "you build it, you run it" the second, and containers shrink the third.

- **Do:** run the same database and queue, at the same major versions, locally with Docker Compose, and
  test against real services (for example with Testcontainers).
- **Don't:** use SQLite in development and Postgres in production. Differences in types, locking and
  SQL then appear only in production.

### 11. Logs: treat them as event streams

The app writes logs, one event per line, to **standard output**. It does not open, rotate or ship log
files. The platform collects the stream: `docker logs`, `kubectl logs`, or an agent such as Fluent
Bit, Vector or the OpenTelemetry Collector sending it to a central store.

```json
{"ts":"2026-10-02T09:14:03Z","level":"info","msg":"order created","request_id":"9f2c"}
```

- **Do:** write structured logs (for example JSON) with a request ID, and set the level from an
  environment variable.
- **Don't:** write to `/var/log/app.log` inside a container. It is lost with the container and can
  fill the disk.

More in [observability: logs, metrics and traces](/posts/observability-logs-metrics-traces).

### 12. Admin processes: run one-off tasks like the app

Migrations, data fixes, a console, "make this user an admin": these run as **one-off processes** with
the same code, dependencies and config as the running release.

- **Do:** keep admin scripts in the repository and run them from the release image:
  `docker compose run --rm app <command>`, a Kubernetes Job, or `heroku run <command>`.
- **Don't:** paste SQL into production from your laptop, or run a local script with different
  library versions against production.

## A real example: this site

This site's code follows most factors, with a few deliberate exceptions.

- **Config:** `src/config.rs` reads all settings from environment variables. Without `DATABASE_URL`
  the app refuses to start. An invalid value such as `COOKIE_SECURE=maybe` stops it with an error
  instead of being guessed. A missing `IP_HASH_SECRET` falls back to a development value with a
  logged warning.
- **Documented, not committed:** `.env.example` documents every variable. The real `.env` is listed in
  `.gitignore` and `.dockerignore`. `src/main.rs` loads it with the `dotenvy` crate if the file
  exists, which is handy in development; in a container the file is simply not there. Variables
  already set in the real environment win over the file.
- **Build:** the `Dockerfile` is multi-stage, compiles with `cargo build --release --locked`, and
  installs `curl` explicitly because the health check uses it.
- **Port, logs, shutdown:** the address comes from `BIND_ADDR` (default `0.0.0.0:3000`). Logs go to
  stdout through the `tracing` and `tracing-subscriber` libraries, filtered by `RUST_LOG` (as
  human-readable text lines, not JSON). On `SIGTERM` or Ctrl+C, the server stops accepting
  connections and lets in-flight requests finish. The app sets no time limit of its own; the
  platform's grace period is the limit.
- **Admin processes:** one-off commands (`make-admin`, `sync-content`) ship in the same binary.

The exceptions: migrations run when the server starts, not as a separate step (the migration tool
takes a Postgres advisory lock, so instances that start together do not collide). Background jobs run
inside the web process (`RUN_BACKGROUND_JOBS=false` turns them off), and jobs that must happen once
take a Postgres advisory lock so several instances do not repeat them. For a small app, that is a
reasonable trade-off.

## Where the manifesto shows its age

The text was written more than ten years ago, from the experience of one platform. What it skips or
simplifies:

- **Telemetry.** Logs are only one signal. Today you also expect metrics, traces and health endpoints
  (liveness and readiness checks), often using OpenTelemetry.
- **Security.** It says little about authentication, authorization, least privilege or the supply
  chain (where your dependencies and images come from). Running as non-root and scanning images for
  known vulnerabilities are now normal.
- **API first.** Other teams depend on your service. Agreeing on the API contract before writing code
  lets teams work in parallel. See [API design](/posts/api-design-pagination-versioning).
- **Richer config.** Large, structured config is often mounted as a file, and settings that change
  without a deploy belong in [feature flags](/posts/feature-flags). The spirit holds: config lives
  outside the build.
- **Stateful systems.** Databases are "just" backing services in the text; running them is harder.

Kevin Hoffman's book *Beyond the Twelve-Factor App* (O'Reilly) revisits the list and adds three
factors: API first, telemetry, and authentication and authorization. In 2024, Heroku announced that it
was making the twelve-factor definition an open-source, community-maintained project, so it can be
updated.

## When not to be strict

The factors are guidelines, not laws. A side project does not need a separate worker deployment. A
WebSocket connection is state inside one process, so you design for reconnects instead (see
[scaling WebSockets](/posts/scaling-websockets-chat)).

## In practice: a quick audit

| Factor | Ask yourself |
|---|---|
| Codebase | Can I name the exact commit running in production? |
| Dependencies | Can a clean CI machine build it from the lockfile alone? |
| Config | Could I publish the repository today without leaking a secret? |
| Backing services | Can I point the app at another database server by changing one variable? |
| Build, release, run | Is the production image exactly the one tested in staging? |
| Processes | Can I kill any instance without users losing data or sessions? |
| Port binding | Does the app start its own server on a port from config? |
| Concurrency | Can I scale web and worker processes separately? |
| Disposability | Does it start in seconds and finish requests on `SIGTERM`? |
| Dev/prod parity | Do I run the same database and versions locally? |
| Logs | Do all logs go to stdout, one event per line? |
| Admin processes | Do migrations and scripts run from the same release as the app? |

## Common mistakes

- A different image per environment, so what you tested is not what you ship.
- A missing required variable that silently falls back to a default instead of stopping the app.
- Secrets printed in startup logs "for debugging".
- Treating the list as complete: an app with no metrics, traces or access control is still hard to run.

## Further reading

- [The Twelve-Factor App](https://12factor.net/): the original text, short enough to read in one sitting
- Kubernetes docs: [ConfigMaps](https://kubernetes.io/docs/concepts/configuration/configmap/) and [Secrets](https://kubernetes.io/docs/concepts/configuration/secret/)
- Kubernetes docs: [Pod Lifecycle](https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/), including how termination works
- Docker docs: [Dockerfile reference](https://docs.docker.com/reference/dockerfile/), including exec form vs shell form
- [OpenTelemetry documentation](https://opentelemetry.io/docs/)
- Kevin Hoffman, *Beyond the Twelve-Factor App* (O'Reilly)
