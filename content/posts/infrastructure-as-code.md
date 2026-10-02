+++
title = "Infrastructure as code: Terraform, OpenTofu and Pulumi without the pain"
summary = "Why clicking in the cloud console stops working, how declarative tools like Terraform, OpenTofu and Pulumi plan and apply changes, and how to handle state, secrets, environments, drift and code review safely."
tags = ["devops"]
level = "intermediate"
date = 2026-10-02
+++

Your company's production runs on AWS. The database, the load balancer, the DNS records and forty
firewall rules were created by hand in the web console three years ago, by someone who has since
left. Now your manager asks for a staging environment "exactly like production". Which settings
matter? Which ones were mistakes? Nobody knows.

**Infrastructure as code (IaC)** solves this. You describe servers, databases, networks and
permissions in text files, keep them in Git, and let a tool create and update the real resources.
This article explains how these tools work and the mistakes that make IaC painful.

## The problem with click-ops

"Click-ops" means managing infrastructure by clicking in a web console, or by typing one-off CLI
commands. It is fast on day one. Over months, it causes these problems:

| Problem | What it looks like |
|---|---|
| No reproducibility | You cannot rebuild production, or create a faithful staging copy. |
| No review | One person changes a firewall rule; nobody checks it first. |
| No history | Nobody knows who opened the database port to the internet, or why. |
| Environments drift apart | Staging and production slowly become different, so tests in staging prove less. |

With IaC, infrastructure gets the same workflow as application code: review, automated checks,
history in Git, and a repeatable way to apply changes. After a
[disaster](/posts/multi-region-and-disaster-recovery), rebuilding means running a tool, not
remembering clicks.

## Declarative vs imperative

An **imperative** approach is a script of steps: "create a bucket, then turn on versioning". Run
it twice and it fails ("bucket already exists") or creates duplicates. If it stops halfway, you
must work out by hand which steps already ran.

A **declarative** approach describes the *end state*: "there is a bucket called `acme-uploads` with
versioning turned on". The tool compares this with what exists and computes the steps itself. If
everything already matches, it does nothing. You know this idea from SQL: you say *which* rows you
want, and the query planner decides *how* to get them. Kubernetes manifests work the same way (see
[from Docker to Kubernetes](/posts/docker-to-kubernetes)).

Terraform, OpenTofu, Pulumi and CloudFormation are all declarative. Pulumi uses a normal
programming language, but the program's result is still a list of desired resources, and an engine
works out the changes.

## How Terraform works

Terraform, and OpenTofu, which works the same way, use a configuration language called **HCL**
(HashiCorp Configuration Language). The core concepts:

- **Provider**: a plugin that talks to one API: AWS, Google Cloud, Azure, Cloudflare, GitHub and
  many more. `terraform init` downloads the providers your code needs.
- **Resource**: one managed object, such as a bucket, a DNS record or a database. When one resource
  refers to another, Terraform learns the order in which to create them.
- **State**: a JSON file that maps each resource in your code (for example `aws_s3_bucket.uploads`)
  to the real object's ID, with its last known attributes. Without it, Terraform cannot know which
  real bucket belongs to which block, or what to delete when you remove a block.
- **Plan**: a preview of the changes needed to make reality match your code.
- **Apply**: carry out the plan by calling the provider APIs, then save the new state.

```text
   .tf files in Git            state file                          real cloud
   (desired state)             (what Terraform manages,            (what exists)
                                last known values)
         |                              |                                |
         +--------------+---------------+                                |
                        v                                                |
                 terraform plan  <------ refresh: read current values ---+
                        |
                        v
      diff:  + create   ~ update in place   - destroy   -/+ replace
                        |
                 a human reads the plan
                        |
                        v
                 terraform apply --------> provider API calls --------> real changes
                        |
                        v
                 state file updated
```

## A small example

This configuration creates a versioned S3 bucket for one environment:

```hcl
terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"   # any 6.x version: pin the major version you have tested
    }
  }
}

provider "aws" {
  region = "eu-west-1"
}

variable "environment" {
  type = string   # "staging" or "prod"
}

resource "aws_s3_bucket" "uploads" {
  bucket = "acme-uploads-${var.environment}"
  tags = {
    Environment = var.environment
    ManagedBy   = "terraform"
  }
}

resource "aws_s3_bucket_versioning" "uploads" {
  bucket = aws_s3_bucket.uploads.id   # a reference: the bucket is created first
  versioning_configuration {
    status = "Enabled"
  }
}
```

The everyday workflow:

```text
$ terraform init                    # download providers, connect to the state backend
$ terraform plan -var environment=staging -out=tfplan
  (output shortened)
  # aws_s3_bucket.uploads will be created
  # aws_s3_bucket_versioning.uploads will be created
  Plan: 2 to add, 0 to change, 0 to destroy.
$ terraform apply tfplan            # apply exactly the plan you reviewed
```

Run the same `plan` command again and it reports no changes: the declarative model at work.

## Remote state and locking

By default, the state is a file called `terraform.tfstate` in your working directory. That breaks as
soon as two people manage the same infrastructure: each has a different copy, and a lost laptop
means a lost state. So teams use a **remote backend**: shared storage such as an S3 bucket, Google
Cloud Storage, Azure Blob Storage, PostgreSQL, or a hosted service like HCP Terraform.

The backend must also support **locking**. Without a lock, two people can run `apply` at the same
time. Both read the same old state, and the last write wins. Resources get created twice or
disappear from the state. With a lock, only one run can change the state at a time. By default, a
second run stops at once with a "state locked" error; with `-lock-timeout=5m` it waits up to five
minutes for the lock instead.

```hcl
terraform {
  backend "s3" {
    bucket       = "acme-terraform-state"
    key          = "billing/prod/terraform.tfstate"
    region       = "eu-west-1"
    encrypt      = true
    use_lockfile = true   # lock file in S3; older setups use a DynamoDB table
  }
}
```

`use_lockfile` (locking with a lock file in the same bucket) needs a recent version: Terraform 1.10
or later, or a recent OpenTofu. On older versions you lock with a DynamoDB table instead
(`dynamodb_table` argument).

Turn on versioning for the state bucket to recover from bad writes. `terraform force-unlock`
removes a lock left by a crashed run; use it only when you are sure no other run is active.

## Secrets end up in the state file

The state stores **every attribute** of every resource, and some are secrets: a database master
password, a value from `random_password`, a private key from `tls_private_key`. They are saved as
plain text in the JSON. Marking a variable or output `sensitive = true` only hides it in the
terminal output. It is still in the state. So:

- Treat the state file as a secret. Encrypt it at rest and limit who can read the bucket.
- Never commit state files to Git. Add `*.tfstate*` and `.terraform/` to `.gitignore`.
- Let the cloud create and keep secrets where possible. For example, with
  `manage_master_user_password = true`, AWS RDS keeps the database master password in AWS Secrets
  Manager, so Terraform never handles it.
- Newer features help. OpenTofu (1.7 and later) can encrypt the state file on your side, before it
  is stored. Terraform added *ephemeral* values (1.10) and *write-only* arguments (1.11), which are
  never saved to state. Write-only arguments only work where the provider supports them.

See [secrets management](/posts/secrets-management) for the wider picture.

## Modules and environments

A **module** is a folder of Terraform files with inputs and outputs. Like a function, you write
"a Postgres database with backups and alarms" once and call it from many places.

```hcl
module "orders_db" {
  source = "../../modules/postgres"
  name   = "orders"
  size   = "medium"
}
```

To keep staging and production apart, Terraform has **CLI workspaces**: one configuration with
several states (`terraform workspace new staging`). They suit short-lived copies, such as one per
feature branch. HashiCorp's documentation warns that they are not a good fit when environments need
separate credentials and access control, as production usually does. Many teams instead use **one
directory per environment**, each with its own state and often its own cloud account:

```text
infra/
  modules/
    network/   postgres/   service/
  envs/
    staging/
      network/   main.tf -> calls modules/network    (own state)
      data/      main.tf -> calls modules/postgres   (own state)
      apps/      main.tf -> calls modules/service    (own state)
    prod/
      network/   data/   apps/                       (same layout, own states)
```

Each environment is also split into smaller states: network, data and apps. This limits the
**blast radius**, meaning how much one mistake can break: a bad change in the app layer cannot
destroy the network. Tools such as Terragrunt reduce the repetition between these folders.

## Drift detection

**Drift** means the real infrastructure no longer matches the code: someone changed a setting in
the console during an incident, or another tool touched the same resource.

`terraform plan` reads current values from the cloud APIs, so drift appears as unexpected changes
in the plan. `terraform plan -refresh-only` shows only what changed outside Terraform. Many teams
run a scheduled plan in CI, for example nightly, with `-detailed-exitcode`: exit code 0 means no
changes, 1 an error, and 2 a difference, which the job turns into an alert. CloudFormation has
built-in drift detection, and Pulumi has `pulumi refresh`.

When you find drift, either **revert** it (apply the code again) or **adopt** it (change the code to
match). If another system owns an attribute, such as the task count your
[autoscaler](/posts/autoscaling) controls, tell Terraform with
`lifecycle { ignore_changes = [desired_count] }`.

## Reviewing plans in pull requests

For infrastructure, the code diff is not enough. A one-line change can destroy and recreate a
database. Reviewers need to see the **plan**.

```text
 developer opens a pull request
        |
        v
 CI: fmt, validate, security scan, plan  -->  plan posted as a PR comment
        |
        v
 reviewer reads the code diff AND the plan
        |
        v
 merge  -->  CI runs apply (only CI has write credentials)
```

What to look for in a plan:

- `-` (destroy) and `-/+` (replace), especially "forces replacement" on databases, buckets, disks
  or DNS zones. Replacing a database gives you a new, empty database.
- Changes to IAM roles, policies and firewall rules (security groups).
- Changes you did not expect, or more than the pull request should cause. They are often drift.

One caveat: the plan in the pull request can be out of date by the time you merge, if other changes
were merged first. The apply step should plan again on the main branch and stop, or ask for review
again, if the new plan differs from the reviewed one.

Atlantis is a popular open-source tool that runs `plan` on pull requests and posts the result as a
comment. HCP Terraform and other commercial platforms offer similar pull request integrations.
Static checkers such as Checkov or Trivy flag risky settings, like a public bucket, before a human
looks. For the pipeline itself, see [CI/CD and hotfixes](/posts/ci-cd-and-hotfixes).

> [!TIP]
> Add `lifecycle { prevent_destroy = true }` to databases and other stateful resources. Any plan
> that would destroy them then fails, and someone must remove the guard on purpose. It does not
> help if someone deletes the whole resource block, because the guard is deleted with it. For
> databases, also turn on the cloud's own deletion protection (for example
> `deletion_protection = true` on an AWS RDS instance).

## Choosing a tool

**Terraform**, first released by HashiCorp in 2014, is widely used and has a very large ecosystem
of providers. It was open source under the Mozilla Public License (MPL 2.0). In August 2023,
HashiCorp announced that new releases would use the Business Source License (BSL 1.1) instead.
The BSL is "source-available": you can read the code, but it is not an open-source licence. Under
HashiCorp's terms you can still use Terraform to manage your own infrastructure; the main
restriction is on offering products that compete with HashiCorp. Terraform 1.5.x was the last
MPL-licensed line.

In response, companies and community members forked the last MPL-licensed code. The fork, first
called OpenTF, was renamed **OpenTofu** and is hosted under the Linux Foundation. Its command is
`tofu` instead of `terraform`, and it reads the same HCL. Most code works unchanged, but the
projects have since diverged in some features.

**Pulumi** uses general-purpose languages such as TypeScript, Python, Go, C# and Java, so you get
loops, functions, packages and unit tests. The risk: infrastructure code can become as clever and
hard to read as any other code.

**CloudFormation** is AWS's own IaC service. You send a JSON or YAML template, AWS keeps the state,
and a failed update is rolled back for you. The **AWS CDK** (Cloud Development Kit) lets you write
code in languages like TypeScript or Python that generates CloudFormation templates.

| | Terraform / OpenTofu | Pulumi | CloudFormation / CDK |
|---|---|---|---|
| Language | HCL | TypeScript, Python, Go, ... | YAML/JSON, or code via CDK |
| What it manages | Many clouds and SaaS products | Many clouds and SaaS products | Mainly AWS |
| State | Your backend, or a hosted service | Pulumi Cloud or your bucket | Kept by AWS |
| Licence | BSL (Terraform), MPL 2.0 (OpenTofu) | Apache 2.0 (open-source engine) | AWS service |

A reasonable default: if you manage several products (a cloud plus DNS, monitoring, GitHub), pick
Terraform or OpenTofu. If you are AWS-only and want no state backend to run, CloudFormation or the
CDK is fine. If your team strongly prefers real programming languages, try Pulumi.

## IaC vs configuration management (Ansible)

Terraform and Ansible are often mentioned together, but they do different jobs.

| | Provisioning (Terraform, OpenTofu, Pulumi) | Configuration management (Ansible, Chef, Puppet) |
|---|---|---|
| Job | Create cloud resources: VMs, networks, databases, DNS | Configure the inside of machines: packages, files, services |
| Talks to | Cloud provider APIs | The machines themselves (Ansible usually uses SSH) |
| State | A stored state file | No state file: checks each machine on every run |

Ansible playbooks are ordered YAML lists of tasks, built from mostly idempotent (safe to repeat)
modules. Some teams use both: Terraform creates the servers, Ansible configures them. Teams that
ship **immutable** artifacts (container images or prebuilt machine images) and replace servers
instead of changing them need much less configuration management.

## When IaC is not worth it

- **Experiments** in a sandbox account. Click freely; write code when the thing becomes real.
- **Very small setups.** One server with Docker Compose may need only a documented setup script
  (see [running production on a single VPS](/posts/deploy-on-a-single-vps)).
- **Application deploys** on every commit. That is the job of your deploy pipeline.

The costs are real (a new language, a state file to protect, provider upgrades), but for shared,
long-lived production infrastructure they pay off.

## Common mistakes

- **Local state.** Kept on a laptop, or committed to Git with the secrets inside. Use a remote
  backend with locking from day one.
- **One giant state.** Thousands of resources in one state make every plan slow (each refresh calls
  cloud APIs, sometimes until you hit rate limits), widen the blast radius, and make everyone wait
  for the same lock. Split by environment and by layer.
- **Manual changes "just this once".** The next apply reverts the console hotfix. The plan does
  show this, but in a long plan it is easy to miss. If you must change something by hand in an
  emergency, put it in code the same day.
- **Renaming without `moved`.** If you rename `aws_db_instance.main` to `aws_db_instance.orders`,
  Terraform sees "delete one database, create another". Add a `moved` block so it only updates the
  state:

  ```hcl
  moved {
    from = aws_db_instance.main
    to   = aws_db_instance.orders
  }
  ```

- **`count` with lists.** Removing an item from the middle of a list shifts every index after it,
  and Terraform changes or recreates those resources. Use `for_each` with stable keys.
- **Unpinned versions.** A new provider major version arrives and your plan changes. Pin versions and
  commit the `.terraform.lock.hcl` file.

## Further reading

- Terraform documentation: [State](https://developer.hashicorp.com/terraform/language/state)
- [OpenTofu documentation](https://opentofu.org/docs/)
- [Pulumi documentation](https://www.pulumi.com/docs/)
- [AWS CDK Developer Guide](https://docs.aws.amazon.com/cdk/v2/guide/home.html)
- [Atlantis](https://www.runatlantis.io/): Terraform pull request automation
- Books: *Terraform: Up & Running* by Yevgeniy Brikman, and *Infrastructure as Code* by Kief Morris
  (both O'Reilly)
