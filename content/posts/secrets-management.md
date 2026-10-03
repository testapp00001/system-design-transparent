+++
title = "Secrets management: keeping passwords and API keys out of trouble"
summary = "What counts as a secret, how to deliver secrets to apps safely, what secret managers and Kubernetes really do, why short-lived credentials beat long-lived keys, and what to do when a secret leaks."
tags = ["security","devops"]
level = "intermediate"
date = 2026-10-02
+++

You join a team and open the repository. In `config/production.yml` you find
`postgres://admin:S3cret!@prod-db:5432/shop`. It was committed four years ago and has been copied to
every laptop, CI cache and fork since. The password has never changed, because nobody knows what
would break. This article explains how to get from there to a setup where a leaked value is boring
instead of scary.

## What counts as a secret

A **secret** is any value that gives access or proves identity. If a stranger had it, they could
read data, spend money or pretend to be you. Examples:

- Database passwords, and connection strings that contain them.
- API keys and tokens for other services (payments, email, cloud providers).
- Private keys: TLS, SSH, and keys that sign JWTs or session cookies.
- OAuth client secrets, [webhook](/posts/webhooks-reliable-delivery) signing secrets, encryption keys.

Not secrets: hostnames, feature flags, public keys, and keys that are *designed* to be public, such
as a payment provider's "publishable" key. A simple test: **would it hurt if this value appeared in a
public screenshot?** If yes, treat it as a secret. Secrets are part of your configuration (see the
[twelve-factor app](/posts/twelve-factor-app)), but need extra rules: who can read them, and how they
are stored and replaced.

## Rule one: secrets never go into Git

Git remembers everything. Deleting a file in a new commit does not remove it from history. Every
clone, fork and CI cache keeps a full copy. Bots scan public repositories all the time, so you should
assume that a secret pushed to a public repository has already been seen.

For local development, keep secrets in a `.env` file that Git ignores, and commit an example file
with fake values so new developers know what to fill in:

```sh
# .gitignore
.env
.env.*
!.env.example

# .env.example  (committed: names only, no real values)
DATABASE_URL=postgres://app:changeme@localhost:5432/app_dev
STRIPE_API_KEY=sk_test_replace_me
```

`.env` files are fine **for local development**, with development-only credentials. They have no
access control, audit log or rotation. On a single server, a `chmod 600` file can be enough (see
[running production on a single VPS](/posts/deploy-on-a-single-vps)). For anything bigger, read on.

### When a secret leaks: rotate first

> [!WARNING]
> Rewriting Git history is **not** a fix. Old clones, forks, cached pages and CI logs still have
> the value. The only real fix is to make the leaked value useless.

1. **Rotate the secret immediately.** Create a new one, deploy it, disable the old one. If the
   logs show that someone is using the key right now, disable it first and accept a short outage.
2. **Check the provider's logs** for use of the old value. Treat it as an
   [incident](/posts/incident-response-and-postmortems), not a cleanup task.
3. **Then, optionally, clean the history** with `git filter-repo` or BFG Repo-Cleaner. This reduces
   future exposure; it does not undo the past.
4. **Find the cause** and add scanning (see below).

## How an application receives a secret

| Method | Pros | Cons |
|---|---|---|
| **Environment variable** | Simple; every language and platform supports it | Inherited by child processes; readable in `/proc/<pid>/environ` (by the same user and root) and in `docker inspect`; may end up in crash reports and debug pages; changing it needs a restart |
| **Mounted file** (e.g. `/run/secrets/db_password`) | File permissions; not inherited; can be updated without a restart if the app re-reads it | The app must read a file; a little more setup |
| **Fetched at runtime** from a secret manager API | No copy on disk; always the current version; reads can be audited | The secret manager becomes a startup dependency; the app needs credentials to call it |

Environment variables are the most common choice and usually acceptable. Files are a little safer
and make rotation easier. Some official Docker images accept both; the PostgreSQL image reads
`POSTGRES_PASSWORD_FILE` as well as `POSTGRES_PASSWORD`. Docker Compose can mount secrets as files:

```yaml
services:
  db:
    image: postgres:17
    environment:
      POSTGRES_PASSWORD_FILE: /run/secrets/db_password
    secrets: [db_password]

secrets:
  db_password:
    file: ./secrets/db_password.txt   # on the server only, never committed
```

Inside the container, Compose makes the secret available as the file `/run/secrets/db_password`.
One caveat: the PostgreSQL image uses this password only when it creates a new, empty database. If
you change the file later, the database password stays the same. To rotate it, change the password
in PostgreSQL itself (for example with `ALTER ROLE`).

## Secret managers

A **secret manager** is a service that stores secrets and gives them only to the right callers. It
offers:

- **Encrypted storage**, often with a master key held in a key management service (KMS).
- **Access policies**: "service `orders-api` in production may read `prod/orders/db`", and nothing else.
- **An audit log**: who read which secret, and when.
- **Versions and rotation**: replace a value without losing the old one immediately.

```text
                     +---------------------------+
                     |      secret manager       |
                     |  encrypted store, access  |
                     |  policies, audit log      |
                     +---------------------------+
                        ^                   |
   2. "I am orders-api" |                   | 3. only the secrets
      (platform token)  |                   |    orders-api may read
                        |                   v
                   +-----------------------------+     4. connect      +----------+
                   |       app: orders-api       | ------------------> | database |
                   +-----------------------------+                     +----------+
                        ^
   1. the platform gives the app an identity
      (Kubernetes service account, cloud VM role, CI OIDC token)
```

Look at step 2. To read secrets, the app must prove who it is. If you solve that with another
password, where do you keep *that* password? This is the **secret zero** problem. The modern answer
is **platform identity**: the cloud, Kubernetes or CI system already knows which workload is running
and gives it a signed, short-lived token. No human handles a long-lived credential.

| Tool | Where it runs | Notes |
|---|---|---|
| HashiCorp Vault | Self-hosted, or managed by HashiCorp | Very flexible: dynamic secrets, many login methods. Running it yourself is real work. OpenBao is an open-source community fork (under the Linux Foundation) created after Vault's 2023 license change. |
| AWS Secrets Manager | AWS | Access via IAM; rotation with Lambda functions, or managed rotation for some AWS services |
| Google Secret Manager | Google Cloud | Access via IAM; versioned secrets; rotation schedules send notifications, you write the rotation code |
| Azure Key Vault | Azure | Secrets, keys and certificates; apps sign in with managed identities |
| Infisical, Doppler | Hosted (Infisical is open source and can be self-hosted) | Developer-friendly UI; CLIs inject secrets as environment variables into a command |

**When not to use one.** A small product on one server does not need Vault; a file with strict
permissions may be enough. And the secret manager becomes a critical dependency: if it is down when
your app starts, your app cannot start. Cache values in memory instead of fetching on every request.

## Kubernetes Secrets are not encrypted by default

A Kubernetes `Secret` looks safe, but its values are only **base64-encoded**. Base64 is a way to write
bytes as text. It is not encryption, and anyone can decode it:

```sh
$ kubectl get secret db-credentials -o jsonpath='{.data.password}' | base64 -d
hunter2
```

In plain (upstream) Kubernetes, the API server stores Secrets unencrypted in etcd, its database,
unless you configure encryption. So:

- **Enable encryption at rest** for Secrets with an `EncryptionConfiguration`, preferably with the
  KMS provider so the key lives outside the cluster. Managed Kubernetes services differ, and some
  encrypt by default; check the documentation of yours.
- **Restrict access with RBAC.** `get`, `list` and `watch` on Secrets all reveal values. Also, anyone
  who can create a Pod in a namespace can mount any Secret in that namespace and read it.
- Secrets mounted as **volumes** are updated in running Pods after a short delay. Secrets used as
  **environment variables** (and volumes mounted with `subPath`) are not; the Pod must restart.

You want your manifests in Git (see [Docker to Kubernetes](/posts/docker-to-kubernetes)), but you
cannot commit a plain Secret. Four common solutions:

| Tool | How it works |
|---|---|
| [External Secrets Operator](https://external-secrets.io/) | Git holds a *reference* ("read `prod/orders/db` from AWS Secrets Manager"). An operator in the cluster fetches the value and creates a normal Secret. |
| Secrets Store CSI Driver | Mounts values from an external secret manager into the Pod as files. |
| [Sealed Secrets](https://github.com/bitnami-labs/sealed-secrets) | `kubeseal` encrypts a Secret with the public key of the Sealed Secrets controller that runs in the cluster. The `SealedSecret` is safe to commit; only that controller has the private keys to decrypt it, so back up those keys. |
| [SOPS](https://github.com/getsops/sops) | Encrypts the values (not the keys) in YAML, JSON or `.env` files with age, PGP or a cloud KMS. Files are committed encrypted and decrypted at deploy time. |

If you already use a cloud secret manager, External Secrets Operator is usually the simplest choice.
It still creates normal Secrets in the cluster, so encryption at rest and RBAC still matter.

## Rotation and short-lived credentials

**Rotation** means replacing a secret with a new value, on a schedule or after an event (someone
leaves, a laptop is lost, a leak). If rotation is scary, it never happens, so make it routine. A
safe rotation uses an **overlap period**, so the app always has a valid credential:

```text
time ------------------------------------------------------------------------>
old secret:  [ valid ....................................... ] revoked
new secret:              [ created ] [ deployed, apps switch ] [ only valid one ...
```

1. Create the new credential while the old one still works: a second API key, or a second database
   user (AWS calls this the "alternating users" strategy).
2. Deploy it; apps pick it up by restarting or re-reading the file.
3. Watch for errors, then revoke the old credential.

In PostgreSQL, the password is checked only when a connection is opened. Changing it does not close
existing connections. Connections that a [connection pool](/posts/connection-pooling) opened with the
old password keep working until they close. This helps during a planned rotation. After a leak it
is a problem: an attacker's open sessions also stay alive. End them with `pg_terminate_backend()`,
for example `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = 'app';`.

Even better than rotating a static secret is not having one. **Dynamic secrets** are created on
demand for one caller and expire automatically. With Vault's database secrets engine, each app
instance asks for credentials and Vault creates a new database user with a time limit (a *lease*):

```sh
$ vault read database/creds/orders-readonly
# returns a new username and password, plus a lease duration.
# When the lease ends, Vault revokes these credentials.
```

You choose the lease length (the TTL, "time to live") in the Vault role. Set it short; Vault's
default is much longer. With a short TTL, a leaked dynamic credential is useful for minutes or
hours, not years. Each one also belongs to a single instance, so the database username tells you
exactly which instance used it.

## No long-lived keys in CI: OIDC federation

The classic CI setup stores a cloud access key in the CI system's secret settings. That key never
expires, and anyone who can change a workflow can send it somewhere. Log masking does not save you:
GitHub, for example, hides the exact value and some common encodings such as base64, but warns that
this is not guaranteed. A value that is changed in another way, or one part of a secret that holds
structured data such as JSON, can still appear in the log.

Modern CI systems can instead prove the job's identity with an **OIDC token**: a short-lived JWT
signed by the CI provider that says "this is repository `my-org/shop`, branch `main`" (see
[OAuth 2.0 and OpenID Connect](/posts/oauth2-and-openid-connect)). The cloud trusts that signer and
exchanges the token for temporary credentials.

```text
GitHub Actions job                         AWS
+-----------------------+  1. signed OIDC token   +-------------------------------+
| permissions:          | ----------------------> | STS checks: issuer, audience, |
|   id-token: write     |                         | sub = repo:my-org/shop:...    |
|                       | <---------------------- +-------------------------------+
|                       |  2. temporary credentials for role "deploy-prod"
|  aws s3 sync ...      | ----------------------> 3. S3, ECS, ...
+-----------------------+
```

```yaml
on:
  push:
    branches: [main]

permissions:
  id-token: write     # allow this job to request an OIDC token
  contents: read

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      # ... checkout and build steps that create ./dist ...
      - uses: aws-actions/configure-aws-credentials@v6
        with:
          role-to-assume: arn:aws:iam::123456789012:role/deploy-prod
          aws-region: eu-west-1
      - run: aws s3 sync ./dist s3://my-shop-assets
```

On the AWS side, the role's trust policy must check the token's `sub` claim, for example
`repo:my-org/shop:ref:refs/heads/main`. Every GitHub repository gets tokens from the same issuer, so
without that check, workflows in other people's repositories could get credentials for your role.
The format of `sub` changes in some cases (for example, a job that uses a GitHub *environment* gets
`repo:my-org/shop:environment:production`), so check GitHub's documentation. Google Cloud
(Workload Identity Federation), Azure (federated credentials) and GitLab CI support the same pattern.

The same idea works at runtime: give a VM or container a cloud identity (an AWS instance profile,
EKS Pod Identity, Workload Identity Federation for GKE, an Azure managed identity). The cloud SDK
then gets short-lived credentials automatically, and there is no key to store.

## Least privilege

Every identity should be able to do only what it needs:

- **One identity per service.** `orders-api` reads only `prod/orders/*`, not every secret.
- **Separate environments.** Staging and production use different secrets, and staging identities
  cannot read production ones.
- **Narrow database users.** A reporting job gets a read-only user, not the owner account.
- **Developers rarely need production secrets.** Use emergency access that is logged and expires.

See [authorization models](/posts/authorization-models) for how to express these rules.

## Secret scanning

People make mistakes, so add a safety net:

- **Before commit:** [gitleaks](https://github.com/gitleaks/gitleaks) runs as a pre-commit hook and
  finds secrets with patterns and entropy checks. Run it in [CI](/posts/ci-cd-and-hotfixes) too, in
  case someone skipped the hook.
- **On the server:** GitHub secret scanning detects many known token formats, and *push protection*
  rejects a push that contains one. For public repositories GitHub also notifies many providers,
  who can revoke the token. For private repositories these features need a paid plan.

Scanning only finds the problem. The fix is still rotation.

## Keeping secrets out of logs and error messages

Secrets often leak through [logs](/posts/observability-logs-metrics-traces), not Git:

- Do not log full request headers; `Authorization` and `Cookie` contain credentials.
- Do not put tokens in URLs. Proxies, access logs and browser history store URLs.
- Some database drivers include the connection string in error messages. Check yours.
- Never return internal errors or stack traces to API clients.

A **secret wrapper type** hides the value when printed. In Python, Pydantic has `SecretStr`:

```python
from pydantic import BaseModel, SecretStr

class Settings(BaseModel):
    db_password: SecretStr

s = Settings(db_password="hunter2")
print(s)                              # db_password=SecretStr('**********')
s.db_password.get_secret_value()      # "hunter2", call this only where needed
```

## Common mistakes

- **Secrets baked into Docker images.** `ARG`, `ENV` and `COPY .env` leave values in image layers and
  history. Use BuildKit secret mounts (`RUN --mount=type=secret,...`) for build-time secrets.
- **Secrets in frontend code.** Anything shipped to a browser or mobile app is public, including
  variables with prefixes such as `NEXT_PUBLIC_` or `VITE_`.
- **Secrets as command-line arguments.** On a default Linux system, other users on the machine can
  see them with `ps`, and they often end up in shell history.
- **Terraform state.** It can contain secrets in plain text; see
  [infrastructure as code](/posts/infrastructure-as-code).
- **Rotation that was never tested.** The first rotation should not happen during an incident.

## Checklist

- [ ] No secrets in Git; `.env` is ignored; scanning runs before commit and in CI.
- [ ] Production secrets live in a secret manager (or a locked-down file on a small server).
- [ ] Each service has its own identity and reads only its own secrets.
- [ ] CI deploys with OIDC federation, not long-lived cloud keys.
- [ ] Kubernetes Secrets are encrypted at rest, and RBAC limits who can read them.
- [ ] Every secret can be rotated without downtime, and you have practised it.
- [ ] Logs and error responses never contain secrets.

## Further reading

- OWASP: [Secrets Management Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html)
- Kubernetes docs: [Good practices for Kubernetes Secrets](https://kubernetes.io/docs/concepts/security/secrets-good-practices/)
- GitHub Docs: [OpenID Connect](https://docs.github.com/en/actions/concepts/security/openid-connect)
- GitHub Docs: [Removing sensitive data from a repository](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)
- HashiCorp Vault docs: [Database secrets engine](https://developer.hashicorp.com/vault/docs/secrets/databases)
