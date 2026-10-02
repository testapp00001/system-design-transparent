+++
title = "From Docker to Compose, Swarm and Kubernetes: what each layer solves"
summary = "Containers and images explained, writing a good Dockerfile, running a stack with Compose, what orchestrators add, Swarm vs Kubernetes, and how to tell whether you actually need Kubernetes."
tags = ["containers", "devops", "backend"]
level = "beginner"
date = 2026-10-02
+++

"It works on my machine" is the problem containers were built to solve. Then came Compose to run
several containers together, and orchestrators like Swarm and Kubernetes to run them across many
machines. Each layer solves a real problem — and adds complexity. This article walks up the ladder
so you can stop at the right rung.

## Containers in one paragraph

A **container** is a normal Linux process that the kernel isolates: it gets its own view of the
filesystem, process list and network (**namespaces**), with limits on CPU and memory (**cgroups**). It
shares the host's kernel, which is why containers start in milliseconds and use far less memory than
virtual machines, which each run a full operating system.

An **image** is the packaged filesystem and metadata a container starts from: your app, its runtime,
libraries, and the command to run. Images are built in **layers**; each instruction in a Dockerfile
adds a layer, and unchanged layers are cached and shared.

```text
 image = [base OS layer] + [runtime layer] + [dependencies layer] + [your code layer] + metadata
 container = running process from that image + its own writable layer (discarded when removed)
```

## A good Dockerfile

```dockerfile
# Build stage: has compilers and build tools
FROM node:22-bookworm AS build
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci                       # cached unless dependencies change
COPY . .
RUN npm run build

# Runtime stage: only what's needed to run
FROM node:22-bookworm-slim
WORKDIR /app
ENV NODE_ENV=production
COPY --from=build /app/package.json /app/package-lock.json ./
RUN npm ci --omit=dev
COPY --from=build /app/dist ./dist
USER node                        # never run as root
EXPOSE 3000
HEALTHCHECK CMD node dist/healthcheck.js || exit 1
CMD ["node", "dist/server.js"]
```

Practices that matter:

- **Multi-stage builds**: compile in a big image, ship a small one. Smaller images pull faster and
  contain fewer vulnerabilities.
- **Order instructions for caching**: copy dependency manifests and install dependencies *before*
  copying source code, so code changes don't reinstall everything.
- **Pin base image versions** (`node:22-bookworm`, not `node:latest`) for reproducible builds.
- **Run as a non-root user.**
- **One process per container**; log to stdout/stderr and let the platform collect logs.
- **Configuration via environment variables**, secrets injected at runtime — never baked into the image.
- A **`.dockerignore`** to keep `node_modules`, `.git` and secrets out of the build context.
- **Handle `SIGTERM`** for graceful shutdown: orchestrators send it before killing a container.

This project's `Dockerfile` follows the same pattern for a Rust app: build in `rust`, run in
`debian-slim` as a non-root user, with a health check.

## Docker Compose: a stack on one machine

Real apps need a database, a cache, maybe a worker. **Compose** describes them in one YAML file and
starts them together with a private network where services find each other by name:

```yaml
services:
  db:
    image: postgres:17
    environment: { POSTGRES_PASSWORD: secret }
    volumes: [ "pgdata:/var/lib/postgresql/data" ]
    healthcheck: { test: ["CMD-SHELL", "pg_isready -U postgres"], interval: 5s }
  app:
    build: .
    environment: { DATABASE_URL: "postgres://postgres:secret@db:5432/postgres" }
    depends_on: { db: { condition: service_healthy } }
    ports: [ "3000:3000" ]
volumes:
  pgdata:
```

`docker compose up` and the whole stack runs. Compose is perfect for **local development**, CI test
environments, and — honestly — **production on a single server** for many small products. A VPS
running Compose with a reverse proxy (Caddy, Traefik, nginx) in front and backups configured goes a
long way.

What Compose doesn't do: run containers across several machines, reschedule them when a machine
dies, or do rolling updates across a fleet.

## Orchestrators: many machines, one desired state

An **orchestrator** takes a description of what you want ("4 replicas of `api:1.8.2`, behind a load
balancer, each with 512 MB RAM") and continuously makes the cluster match it:

- **Scheduling**: place containers on machines with enough resources.
- **Self-healing**: restart crashed containers; reschedule containers from failed machines.
- **Service discovery and load balancing** between containers.
- **Rolling updates and rollbacks.**
- **Scaling**, manual or automatic.
- **Config and secrets** distribution; **storage** attachment.

### Docker Swarm

Swarm mode is built into Docker. You can reuse Compose files (`docker stack deploy`) and get multi-host
services, rolling updates, secrets and an overlay network with relatively little to learn. It's much
simpler than Kubernetes, but has a far smaller ecosystem and community momentum. It's a reasonable
choice for small teams that have outgrown one server and want to stay simple.

### Kubernetes

The industry standard orchestrator, originally from Google, now governed by the CNCF. You describe
resources in YAML:

- **Pod**: one or more containers scheduled together (usually one).
- **Deployment**: keeps N replicas of a pod template running; does rolling updates.
- **Service**: a stable name and virtual IP load-balancing across matching pods.
- **Ingress / Gateway API**: HTTP routing from outside into Services.
- **ConfigMap / Secret**: configuration and credentials.
- **Job / CronJob**: run-to-completion and scheduled work.
- **HorizontalPodAutoscaler**: scale replicas on CPU or custom metrics.
- **StatefulSet + PersistentVolume**: stateful workloads with stable identity and storage.

Kubernetes' strengths: a huge ecosystem (Helm charts, operators for databases and queues, service
meshes, GitOps tools like Argo CD and Flux), portability across clouds, and a consistent way to run
everything. Its cost: a lot of concepts, a lot of YAML, and real operational work — upgrades,
networking, security, monitoring of the cluster itself. Managed offerings (EKS, GKE, AKS and others)
take away the control-plane work but not the learning curve.

## Do you need Kubernetes?

Probably **not yet** if:

- You run a handful of services with modest traffic.
- One or two servers (with Compose) or a PaaS (Render, Fly.io, Railway, Heroku, Cloud Run, App
  Runner…) can run everything.
- Nobody on the team has operated it, and there's no platform team.

Probably **yes** (or soon) if:

- You run many services and teams that need a consistent deployment platform.
- You need autoscaling, self-healing and rolling deploys across many machines.
- You want to standardise on the cloud-native ecosystem and can invest in learning it, or use a
  managed service.

A common, healthy progression:

```text
 one VPS + Compose  ->  PaaS or a few VMs + Compose/Swarm  ->  managed Kubernetes + GitOps
     (weeks)                    (months/years)                  (when the org needs a platform)
```

Each step should be driven by a concrete pain, not by the résumé. Many profitable products never
leave the first two rungs.

## Common mistakes

- Treating containers like VMs: SSH-ing in, editing files, expecting changes to persist.
- Huge images built from full OS images with compilers included.
- Secrets in images or committed `.env` files.
- Running databases in containers without understanding volumes and backups — the container is
  disposable, the data must not be.
- No resource limits, so one container starves the others (and no requests in Kubernetes, so the
  scheduler can't place pods sensibly).

## Further reading

- Docker docs: [Dockerfile best practices](https://docs.docker.com/build/building/best-practices/) and [multi-stage builds](https://docs.docker.com/build/building/multi-stage/)
- Docker docs: [Compose file reference](https://docs.docker.com/reference/compose-file/)
- Docker docs: [Swarm mode overview](https://docs.docker.com/engine/swarm/)
- [Kubernetes documentation: Concepts](https://kubernetes.io/docs/concepts/)
- [The Twelve-Factor App](https://12factor.net/)
