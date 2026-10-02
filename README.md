# System Design Transparent

**An open, community-written knowledge base about how real backends are built** — the things you
usually only learn if you are lucky enough to work at a company that already does them.

Retries and idempotency keys. Why MySQL and PostgreSQL store rows differently and how that changes
performance. Connection pools, replication, backups, caching, load balancing, scaling WebSockets,
background jobs, CI/CD and hotfixes, Docker to Kubernetes, monoliths and microservices, REST vs gRPC vs
GraphQL, choosing a database. Explained clearly, with trade-offs, for free.

- **Read, search and filter** articles by tag, level and popularity (PostgreSQL full-text search).
- **Three themes**: light, dark and a sepia *reader* mode with serif text and no distractions.
- **Roadmap**: a map of everything a backend engineer should know exists — including topics nobody
  has written yet.
- **Vote rounds**: an admin opens a round (e.g. 3 days or a week) for a few tags; anyone can suggest
  the next article and vote, **no account needed** (limits are per network/IP).
- **Optional accounts** (username + password only) to like, upvote and save articles.
- **RSS feed** and sitemap so the knowledge is discoverable.

## Tech stack

| Layer      | Choice | Why |
|------------|--------|-----|
| Server     | Rust + [axum](https://github.com/tokio-rs/axum) | Fast, small memory footprint, one static binary |
| Templates  | [Askama](https://github.com/askama-rs/askama) | Compiled and type-checked at build time |
| Interactivity | [htmx](https://htmx.org) (vendored, no CDN) | Server-rendered HTML; works without JS too |
| Database   | PostgreSQL 16+ | Full-text search, trigram typo tolerance, advisory locks, one dependency |
| Content    | Markdown files in `content/` | Git is the CMS: anyone can improve an article with a pull request |

No Redis, no queue, no SPA framework. The site is deliberately small so it can be read in an
afternoon — and it uses, in miniature, several patterns the articles explain (see
[About](templates/about.html) or the doc comments in `src/`).

## Quick start (Docker)

```sh
docker compose up --build
# open http://localhost:3000, register an account, then make yourself admin:
docker compose exec app app make-admin <your-username>
```

## Local development

Requirements: Rust (stable, 1.94+) and PostgreSQL 14+ (with the `pg_trgm` extension, included in the
standard packages).

```sh
cp .env.example .env               # edit DATABASE_URL
createdb sdt                       # or: docker compose up -d db
cargo run                          # migrates, syncs content/, serves on :3000
cargo run -- make-admin alice      # after registering "alice" on the site
```

On startup the server runs SQL migrations, validates every file in `content/`, upserts changed
articles into Postgres and starts serving. Edit a Markdown file and restart to see the change.

### Commands

```
system-design-transparent serve              # default
system-design-transparent sync-content       # migrate + sync content, then exit
system-design-transparent check-content      # validate content/ without a database (CI)
system-design-transparent make-admin <user>  # grant admin rights
```

### Tests

```sh
# Integration tests create a throwaway database per test on this server.
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The integration tests drive the real router against real Postgres: search, auth, idempotent
reactions, vote limits (including a concurrency test that fires simultaneous votes to prove the
per-IP cap cannot be raced), admin authorisation and CSRF protection.

## Project layout

```
content/
  posts/*.md        articles (TOML frontmatter + Markdown)
  tags.toml         every tag must be declared here
  roadmap.toml      the knowledge map shown on /roadmap
migrations/         SQL schema (commented — the schema is part of what the project teaches)
src/
  main.rs           CLI + server startup + graceful shutdown
  routes/           HTTP handlers (parse input → call domain → render)
  posts.rs          listing, full-text search, reactions
  votes.rs          vote rounds, suggestions, race-free limits
  auth.rs           Argon2id passwords, server-side sessions
  content/          Markdown rendering (anchors, TOC) and sync into Postgres
  security.rs       CSRF protection via Fetch Metadata, security headers
  ip.rs             client IP resolution behind proxies, IPv6 /64 grouping
  worker.rs         background jobs with advisory locks
templates/          Askama HTML templates
static/             CSS (3 themes), htmx, tiny theme script
tests/app.rs        integration tests
```

## Configuration

All configuration is via environment variables — see [`.env.example`](.env.example) for the full,
documented list. The important ones for production:

| Variable | Notes |
|----------|-------|
| `DATABASE_URL` | Required. |
| `IP_HASH_SECRET` | Long random string. Voter IPs are stored only as keyed hashes. |
| `TRUSTED_PROXY_HOPS` | Number of proxies in front of the app (usually `1`). |
| `COOKIE_SECURE` | `true` when served over HTTPS. |
| `PUBLIC_URL` | Used in the RSS feed and sitemap. |

## Deploying

The app is a single stateless binary plus Postgres, so anything that runs a container works
(a small VPS with Docker Compose and Caddy in front is plenty). Run as many instances as you like
behind a load balancer: sessions live in Postgres, migrations and content sync take advisory locks,
and background jobs run exactly once across instances.

## Contributing

Articles are the heart of the project. Read [CONTRIBUTING.md](CONTRIBUTING.md) for the article
template, style guide and sourcing rules. Code contributions are welcome too.

## License

Code: [MIT](LICENSE).
