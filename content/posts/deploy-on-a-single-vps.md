+++
title = "Running production on a single VPS: Docker Compose, Caddy, backups and safe deploys"
summary = "How to run a small product safely on one server: basic hardening, Docker Compose, Caddy with automatic HTTPS, secrets, PostgreSQL backups off the server, log rotation, monitoring, and a deploy script with health checks and rollback."
tags = ["devops","containers","backend"]
level = "beginner"
date = 2026-10-02
+++

Your app works on your laptop: one `docker compose up` starts the app and its database. Now real
users are coming. Kubernetes feels like too much, and a platform as a service (PaaS) gets expensive.
The third option is one rented virtual server (a **VPS**, virtual private server) running the same
Compose stack. It is cheap and simple, but now *you* own security updates, HTTPS, backups and deploys.

This article shows how to do it properly, with this website as the example. Its README says "a
small VPS with Docker Compose and Caddy in front is plenty".

## Why one server goes a long way

A modest VPS with a few CPU cores and a few gigabytes of memory can serve much more traffic than
most new products receive. Small apps usually struggle because of
[slow queries](/posts/database-indexes-and-explain), not because of too few servers.

| You get | You give up |
|---|---|
| Low cost, one bill | **No redundancy**: if the machine dies, the site is down until you rebuild it |
| One place to look when something breaks | You do the updates, backups and monitoring |
| App and database close together: low latency | Growth stops at the biggest server you can rent |
| Easy to understand end to end | Reboots and deploys can cause short downtime |

One server is a good choice when **a few minutes of downtime, a few times a year, is acceptable**,
and your backups let you rebuild quickly.

## The setup at a glance

```text
                            Internet
                               |
                   firewall: 22, 80, 443 only
                               |
+------------------------------|-----------------------------+
| VPS                          v                             |
|   [ caddy ]  ports 80 and 443: HTTPS certificates, proxy   |
|       |                                                    |
|       |  http://app:3000   (private Compose network)       |
|       v                                                    |
|   [ app ]    stateless container, answers GET /healthz     |
|       |                                                    |
|       |  postgres://db:5432                                |
|       v                                                    |
|   [ db ]     PostgreSQL, files in named volume "pgdata"    |
|                                                            |
|   cron, nightly: pg_dump ----------------------------------+---> object storage
+------------------------------------------------------------+     (another provider)
          ^
          |  every minute: GET https://example.com/healthz
   external uptime monitor (on another machine)
```

## Step 1: harden the server

First, on a fresh Debian or Ubuntu server, as root:

```sh
# a normal user with sudo, using the SSH key you logged in with
adduser deploy && usermod -aG sudo deploy
mkdir -p /home/deploy/.ssh && cp ~/.ssh/authorized_keys /home/deploy/.ssh/
chown -R deploy:deploy /home/deploy/.ssh

# SSH keys only, no root login
printf 'PasswordAuthentication no\nPermitRootLogin no\n' > /etc/ssh/sshd_config.d/00-hardening.conf
sshd -T | grep -E 'passwordauthentication|permitrootlogin'   # the values really used
systemctl restart ssh

# firewall (SSH, HTTP, HTTPS only) and automatic security updates
apt update && apt install ufw unattended-upgrades
ufw default deny incoming
ufw allow 22/tcp && ufw allow 80/tcp && ufw allow 443/tcp
ufw enable
dpkg-reconfigure --priority=low unattended-upgrades
```

Some cloud images ship a file in `/etc/ssh/sshd_config.d/` that turns password login back on, so
check with `sshd -T`. Keep your SSH session open and test the new login from a second terminal.
Kernel updates need a reboot; on Ubuntu, `/var/run/reboot-required` tells you when.

> [!WARNING]
> **Docker bypasses ufw.** Docker writes its own firewall rules. A port published with
> `ports: "3000:3000"` is reachable from the internet even if ufw blocks it. Publish only Caddy's
> ports. For anything else, publish nothing, or bind to `127.0.0.1` as the repository's `db`
> service does (`"127.0.0.1:5432:5432"`).

## Step 2: Docker and a good image

Install Docker Engine from Docker's official package repository. It includes the Compose plugin
(`docker compose`, with a space).

- The `docker` group is **equivalent to root**: its members can mount any host folder into a
  container. Treat it like `sudo`.
- **Build images in CI, not on the server**, so compiling does not steal CPU and memory from
  production. This project's CI already builds the image (without publishing it); for production,
  also push it to a registry, tagged with the git commit.

The repository's `Dockerfile` shows three habits worth copying:

- **Multi-stage build**: compile in `rust:1-bookworm`, ship only the binary, static files and content
  in `debian:bookworm-slim`. Small images download faster and contain less to attack.
- **Non-root user** (`USER app`): an attacker who breaks into the app is not root in the container.
- **A health check** on `/healthz`, which runs `SELECT 1` on the database. "Healthy" means "can
  serve pages", not just "the process exists".

## Step 3: a production Compose file with Caddy

The repository's `docker-compose.yml` is for a **local** stack (its first comment says so). For
production, five things change:

| Local file | Production |
|---|---|
| `build: .` | an image built by CI, tagged `${APP_TAG}` |
| password `sdt`, `IP_HASH_SECRET: change-me-in-production` | real secrets from `.env` |
| app publishes `"3000:3000"` | no published port: only Caddy reaches the app |
| `TRUSTED_PROXY_HOPS: "0"`, `COOKIE_SECURE: "false"` | `"1"` and `"true"`, as the README says |
| no proxy | a `caddy` service on ports 80 and 443 |

```yaml
# /opt/sdt/compose.yaml
services:
  db:
    image: postgres:17
    environment:
      POSTGRES_USER: sdt
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?set it in .env}
      POSTGRES_DB: sdt
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U sdt -d sdt"]
      interval: 5s
      retries: 20
    restart: unless-stopped

  app:
    image: registry.example.com/sdt:${APP_TAG:?set it in .env}
    depends_on:
      db: { condition: service_healthy }
    environment:
      DATABASE_URL: postgres://sdt:${POSTGRES_PASSWORD}@db:5432/sdt
      IP_HASH_SECRET: ${IP_HASH_SECRET:?set it in .env}
      PUBLIC_URL: https://example.com
      TRUSTED_PROXY_HOPS: "1"
      COOKIE_SECURE: "true"
    restart: unless-stopped

  caddy:
    image: caddy:2
    ports: ["80:80", "443:443", "443:443/udp"]   # udp: HTTP/3
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data          # certificates and keys: keep them
      - caddy_config:/config
    restart: unless-stopped

volumes:
  pgdata:
  caddy_data:
  caddy_config:
```

`restart: unless-stopped` brings containers back after a crash or a reboot. The `Caddyfile` is the
README's minimal one, and these three lines do a lot:

```text
example.com {
    reverse_proxy app:3000
}
```

- **Automatic HTTPS.** For a domain name, Caddy gets a free certificate from a certificate authority
  such as Let's Encrypt, renews it, and redirects HTTP to HTTPS. The domain's
  [DNS record](/posts/dns-for-backend-developers) must already point to the server, and ports 80
  and 443 must be reachable.
- **Reverse proxy.** Caddy forwards requests to `app` over the private Compose network. It keeps the
  original `Host` header and adds `X-Forwarded-For`. `TRUSTED_PROXY_HOPS=1` tells the app that
  exactly one proxy is in front. Too low, and every visitor looks like Caddy; too high, and visitors
  can fake their IP address.
- **Keep `caddy_data`.** It stores the certificates. Lose it, and Caddy must request new ones, which
  can hit the certificate authority's rate limits.

More in [reverse proxies and TLS termination](/posts/reverse-proxies-and-tls).

## Step 4: environment and secrets

Compose reads a file named `.env` next to the Compose file and fills in the `${...}` values. Keep
production secrets there, **on the server only**:

```sh
# /opt/sdt/.env   (chmod 600, never committed to git)
APP_TAG=3f9c2e1
POSTGRES_PASSWORD=...   # generate with: openssl rand -hex 32
IP_HASH_SECRET=...      # generate with: openssl rand -hex 32
```

- `${VAR:?message}` makes Compose refuse to start when a value is missing, instead of using an
  empty string.
- Hex values are safe inside `DATABASE_URL`; characters such as `@` or `/` would need URL encoding.
- Keep a copy of this file in a password manager: backups do not contain it.
- `POSTGRES_PASSWORD` is only used on the very first start, when the volume is empty. To change the
  password later, use SQL (`ALTER USER`).

See also [secrets management](/posts/secrets-management).

## Step 5: PostgreSQL data and off-server backups

The named volume `pgdata` survives restarts, new images and `docker compose down`. Two things
destroy it: `docker compose down -v` (the `-v` deletes volumes), and losing the server's disk, where
the volume lives. So backups must leave the machine. A solid start is a nightly `pg_dump`, uploaded
to **object storage** (S3-compatible storage) at another provider, for example with `rclone`:

```sh
#!/usr/bin/env bash
# /opt/sdt/backup.sh, run by cron every night:  15 3 * * * /opt/sdt/backup.sh
set -euo pipefail
cd /opt/sdt
file="backups/sdt-$(date -u +%Y-%m-%dT%H%M).dump"
# -T: no terminal, so the binary dump is not corrupted
docker compose exec -T db pg_dump -U sdt --format=custom sdt > "$file"
rclone copy "$file" offsite:sdt-backups/    # "offsite" is a remote in rclone's config
rm "$file"
curl -fsS --retry 3 https://monitor.example.com/ping/backup > /dev/null
```

Make the backups trustworthy:

- **Alert when the job does not run.** The last line pings a "dead man's switch" monitor
  (Healthchecks.io is one example). If no ping arrives for a day, it alerts you.
- **Use upload-only credentials.** If the server's storage key cannot delete, an attacker on the
  server cannot destroy your backups. Let the bucket's lifecycle rules remove old files.
- **Encrypt** the dump if the storage provider should not read your data (an rclone `crypt` remote,
  `age` or `gpg`).
- **Test a restore** every month with `pg_restore --dbname=sdt_restore_test <file>`. An untested
  backup is only a hope.

**When to add WAL archiving.** With a nightly dump, you can lose up to 24 hours of data. PostgreSQL
writes every change to its write-ahead log (WAL) first. Tools such as pgBackRest and WAL-G copy the
WAL to object storage continuously, so you can restore to any moment, for example just before a bad
`DELETE`. Add it when losing a day of data is not acceptable, or when the dump gets slow. Managed
PostgreSQL services usually include it. See
[database backups and recovery](/posts/database-backups-and-recovery).

> [!WARNING]
> Changing `postgres:17` to `postgres:18` does not upgrade your data: a new major version refuses to
> start on the old files. Upgrade with a dump and restore (or `pg_upgrade`), after a fresh backup.

## Step 6: logs and disk space

Docker's default logging driver (`json-file`) does **not rotate logs**. They grow until the disk is
full, and a full disk breaks PostgreSQL. Fix it once in `/etc/docker/daemon.json`:

```json
{
  "log-driver": "local",
  "log-opts": { "max-size": "10m", "max-file": "5" }
}
```

Restart Docker, then recreate the containers (`docker compose up -d --force-recreate`): the setting
only applies to new containers.

Deploys leave old images behind. A weekly `docker image prune -a --filter "until=240h"`
deletes unused images older than ten days, never one that a container uses.

## Step 7: monitoring that warns you before users do

Start with three alerts:

| What | How |
|---|---|
| Site is up | Request `https://example.com/healthz` every minute. This tests DNS, TLS, Caddy, the app and the database |
| Disk space | Alert at about 80% full |
| Backups ran | The dead man's switch from step 5 |

A monitor on the same server cannot tell you that the server is dead. Use a hosted uptime service,
or a self-hosted tool such as Uptime Kuma on a different machine. For disk space, a cron job that
checks `df` is enough. Add more [observability](/posts/observability-logs-metrics-traces) later.

## Step 8: a deploy script you can trust

```text
 git push -> CI: test, build, push image sdt:<commit sha>
 server: ./deploy.sh <sha>
   1. pull the new image          (old version still serving)
   2. run database migrations     (old version still serving)
   3. replace the app container   (a few seconds of switch-over)
   4. check /healthz              -> OK: done  |  failed: start the previous tag again
```

Tag images with the **commit SHA**, not `latest`, so you always know what is running, and a
rollback just runs the previous tag.

```sh
#!/usr/bin/env bash
# /opt/sdt/deploy.sh <tag>      example: ./deploy.sh 3f9c2e1
set -euo pipefail
cd /opt/sdt
new="$1"
old=$(grep '^APP_TAG=' .env | cut -d= -f2)

rollback() {
  echo "deploy failed, going back to $old" >&2
  sed -i "s/^APP_TAG=.*/APP_TAG=$old/" .env
  docker compose up -d app
  exit 1
}

sed -i "s/^APP_TAG=.*/APP_TAG=$new/" .env
docker compose pull app || rollback
docker compose run --rm app app sync-content || rollback   # migrations, then exit
docker compose up -d app

for i in $(seq 1 30); do
  curl -fsS --max-time 3 https://example.com/healthz > /dev/null && { echo "deployed $new"; exit 0; }
  sleep 2
done
rollback
```

- **Migrations run before the switch.** The app also migrates when it starts, but `sync-content`
  migrates and exits. If a migration fails, the deploy stops while the old version still serves.
- **A rollback changes the code, not the database.** The old version must still work with the new
  schema: add columns first, remove them in a later deploy (see
  [zero-downtime migrations](/posts/deployment-strategies-and-zero-downtime-migrations)).
- [CI](/posts/ci-cd-and-hotfixes) can run this script over SSH, or you can run it by hand.

## Reducing downtime during deploys

`docker compose up -d app` stops the old container, then starts the new one. In between, nothing
listens, and Caddy answers `502 Bad Gateway`. From cheap to more work:

1. **Do slow work before the switch.** The script pulls and migrates first.
2. **Shut down gracefully.** Docker sends `SIGTERM`, waits 10 seconds by default, then kills the
   process. This app catches `SIGTERM` and lets running requests finish (`src/main.rs`).
3. **Let Caddy wait instead of failing.** With `lb_try_duration`, Caddy keeps trying to connect for a
   while, so requests during a short restart are only slower:

   ```text
   example.com {
       reverse_proxy app:3000 {
           lb_try_duration 10s
       }
   }
   ```

4. **Two app containers.** Run `app_blue` and `app_green`, list both in `reverse_proxy` with an
   active health check (`health_uri /healthz`), and update them one at a time. Caddy only sends
   traffic to healthy ones. This app supports it: its README says sessions live in Postgres, and
   migrations and background jobs use advisory locks. Tools such as Kamal automate this pattern.

For many small products, a few seconds of errors at a quiet hour is fine. Do step 4 when you deploy
often during busy hours.

## When to graduate to more servers

Change the setup when a real problem appears:

| Signal | Usual next step |
|---|---|
| CPU or memory often high | Fix slow queries, then rent a **bigger server** |
| App and database fight for memory or disk | Move PostgreSQL to a **managed database** or its own server |
| Downtime from one dead server is no longer acceptable | **Two or more app servers** behind a load balancer, plus a database replica with failover |
| Many services and teams deploying all day | A PaaS, Docker Swarm or managed Kubernetes |

See [load balancing](/posts/load-balancing-and-stateless-servers),
[replication](/posts/replication-and-high-availability) and
[Docker to Kubernetes](/posts/docker-to-kubernetes). Before any of that, write down how to rebuild
the server from nothing, ideally as [infrastructure as code](/posts/infrastructure-as-code), and
practise it once.

## Common mistakes

- Publishing the app or database port and trusting ufw to block it.
- Backups only on the same server, or never test-restored.
- Deploying `latest`, so you cannot tell what runs or roll back reliably.
- Forgetting `TRUSTED_PROXY_HOPS` and `COOKIE_SECURE` behind the proxy.
- A destructive migration in the same deploy as the code change: rollback becomes impossible.

## Further reading

- Caddy docs: [Automatic HTTPS](https://caddyserver.com/docs/automatic-https)
- Caddy docs: [reverse_proxy directive](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)
- Docker docs: [Packet filtering and firewalls](https://docs.docker.com/engine/network/packet-filtering-firewalls/)
- Docker docs: [Configure logging drivers](https://docs.docker.com/engine/logging/configure/)
- PostgreSQL docs: [pg_dump](https://www.postgresql.org/docs/current/app-pgdump.html)
- Debian wiki: [UnattendedUpgrades](https://wiki.debian.org/UnattendedUpgrades)
