+++
title = "Multi-region architecture and disaster recovery"
summary = "What RPO and RTO really mean, zones vs regions, the four classic DR strategies from backup-and-restore to active-active, how data and traffic move between regions, the hidden single points of failure, and when one region is enough."
tags = ["reliability","devops","distributed-systems"]
level = "advanced"
date = 2026-10-02
+++

It is 3 a.m. Your cloud provider's status page says "increased error rates" in the region where
everything you own runs. Ten minutes later someone from the business asks two questions: **"When will
we be back?"** and **"Did we lose any orders?"** If the honest answer to both is "we don't know", this
article is for you.

**Disaster recovery (DR)** is the plan for getting a service back after a large failure: a lost data
center, a whole region, a deleted database. A **multi-region architecture** can make that plan fast,
but it is expensive and full of traps. The goal is to choose the *right amount* of it.

## RPO and RTO: the two numbers behind every decision

The two questions above have names:

- **RPO (recovery point objective)**: the maximum data loss you accept, measured in time. An RPO of
  5 minutes means "we may lose at most the last 5 minutes of writes".
- **RTO (recovery time objective)**: the maximum downtime you accept, from the start of the disaster
  until the service works again.

```text
  last copy that reached     disaster              service works
  the other region           happens               again
        |                       |                      |
 -------+-----------------------+----------------------+--------> time
        |<----- data lost ----->|<----- downtime ----->|
               (keep <= RPO)          (keep <= RTO)
```

Three things people often miss:

1. **RTO includes people.** Noticing, paging, deciding, acting and checking all count. A two-minute
   failover script does not give a two-minute RTO if the decision takes forty.
2. **They are business decisions.** Engineers estimate the cost; the business decides what an hour of
   downtime is worth.
3. **Targets differ per part.** Checkout may need an RPO of seconds; analytics can wait for last
   night's backup.

The database side is covered in [replication and high
availability](/posts/replication-and-high-availability) and [backups and point-in-time
recovery](/posts/database-backups-and-recovery).

## Failure domains: availability zones vs regions

A **failure domain** is a group of things that can fail together. AWS, Google Cloud and Azure all
offer two levels, with differences in detail:

- An **availability zone (AZ)** is one or more data centers inside a region, with separate power,
  cooling and networking. Round trips between zones of one region are short, typically a few
  milliseconds or less.
- A **region** is a geographic area (for example Frankfurt) containing several zones. Regions are far
  apart and run mostly independent infrastructure.

| What fails | What protects you |
|---|---|
| A server or disk | Several instances, database replicas |
| A zone (power, cooling, network cut) | Running in two or more zones (multi-AZ) |
| A whole region | A second region |
| Your data itself (`DROP TABLE`, ransomware) | Backups with point-in-time recovery, in another account and region |
| Your own bad deploy or config | Gradual rollouts and fast rollback, not more regions |

The key physical difference is **latency**. Light in optical fibre covers about 200 km per
millisecond, so every 100 km of cable adds roughly 1 ms to a round trip. Zones are close, so
synchronous replication between them is common. Across regions, round trips take tens of milliseconds
within a continent and often over 100 ms between continents.

## The four classic DR strategies

AWS's disaster recovery guidance names four strategies, from cheapest and slowest to most expensive
and fastest, and places their RPO/RTO at roughly hours, tens of minutes, minutes and near real time.

| Strategy | Running in the recovery region before the disaster | Typical RTO | RPO depends on | Idle cost |
|---|---|---|---|---|
| Backup and restore | Only copies of backups | Hours | Age of the last copied backup | Lowest |
| Pilot light | Data stores, replicating live; app servers off | Tens of minutes | Replication lag | Low |
| Warm standby | A complete but scaled-down copy of production | Minutes | Replication lag | Medium |
| Multi-site active-active | Full production, serving real users | Near zero | Replication lag, or zero if writes are synchronous | Highest |

**Backup and restore.** Backups are copied to another region, and the infrastructure is written as
code so it can be recreated there ([infrastructure as code](/posts/infrastructure-as-code)). Cheap, but
rebuilding everything takes hours, and you only know how many if you have tried.

**Pilot light.** Named after the small flame that keeps a gas heater ready. The data, the hardest part
to move, replicates continuously; app servers are off until you need them.

**Warm standby.** Everything runs in the recovery region, only smaller. It can take traffic at once
while it scales up, and you can test it all the time.

```text
                                     users
                                       |
            [ global routing: DNS, anycast or global load balancer ]
                |                                               :
                | 100% of traffic                               : 0% until failover
                v                                               v
  +--- Region A (primary) ---+                   +--- Region B (warm standby) ----+
  | app servers x 20         |                   | app servers x 2                |
  |                          |                   | (scaled up on failover)        |
  | DB primary               |  --- async ---->  | DB replica (a bit behind)      |
  | object storage           |  --- copy ----->  | object storage replica         |
  +--------------------------+                   +--------------------------------+
```

**Multi-site active-active.** Every region serves real users all the time. It is the fastest option
and the hardest, because writes from several regions must be reconciled (see below). Capacity rule:
with N equal regions, each should normally use at most (N-1)/N of its capacity, so the survivors can
absorb a lost region: 50% with two regions, 75% with four. Planning to [autoscale](/posts/autoscaling)
instead? Other customers of the failed region may be doing the same.

## Getting the data there: replication and its lag

Stateless app servers are easy to run anywhere. Data is the hard part.

Most cross-region database replication is **asynchronous**: the primary confirms a commit, then ships
the change. The gap is the **replication lag**, and it is your real RPO: anything not yet replicated
when the region dies is lost, or stuck there until it returns. Lag grows during write spikes
(backfills, migrations) and when the network between regions is degraded, which is exactly what tends
to happen during an incident. Measure it and alert on it:

```sql
-- On the primary (PostgreSQL 10+): how far behind is each replica?
SELECT application_name, state, replay_lag FROM pg_stat_replication;

-- On the replica in the recovery region:
SELECT now() - pg_last_xact_replay_timestamp() AS since_last_replayed_commit;
```

The second value also grows when the primary is idle; a heartbeat row updated every few seconds fixes
that.

The alternative is **synchronous** cross-region writes, where a commit succeeds only once other regions
have it. Google Spanner and CockroachDB do this with consensus
([consensus and leader election](/posts/consensus-and-leader-election)). Committed data survives a
region failure, but every write pays cross-region round trips.

Remember data outside the main database:

| Data | Typical approach |
|---|---|
| Object storage (uploads) | Cross-region bucket replication (also async) |
| Queues and event streams | Replicate topics (Kafka MirrorMaker 2), or rebuild from the database |
| Caches | Usually not replicated; expect a cold cache to hit the database hard ([caching](/posts/caching-strategies)) |
| Secrets and encryption keys | Must already exist in the recovery region |

> [!WARNING]
> Replication copies mistakes too: a `DELETE` without `WHERE` reaches the other region within seconds.
> Keep [backups](/posts/database-backups-and-recovery), with at least one copy in an account that
> production credentials cannot delete.

## Sending users to the right region

When a region fails, traffic must move. Three mechanisms are common, often combined:

- **DNS failover.** Your DNS provider health-checks each region and answers with a healthy one's
  address. Simple and cheap, but resolvers cache answers for the record's TTL (time to live), and some
  clients cache longer, so a tail keeps hitting the dead region
  ([DNS for backend developers](/posts/dns-for-backend-developers)).
- **Anycast.** The same IP address is announced from many locations through BGP, the routing protocol
  between internet networks. Packets go to the nearest location in network terms. If a location stops
  announcing, routers send traffic elsewhere, with no DNS cache involved.
- **Global load balancers.** Google Cloud's global external Application Load Balancer, AWS Global
  Accelerator, Azure Front Door and Cloudflare Load Balancing give one stable entry point that
  health-checks your regions and forwards requests to a healthy one. Failover then happens inside the
  provider, without waiting for DNS caches.

Health checks decide everything, so check from **several outside locations**, test something
**meaningful** (but not so deep that one minor dependency evacuates a region), and require **several
failures in a row** before failing over, and several successes before failing back.

## Active-active: what happens when both regions write

Reads are easy to serve everywhere. Writes are not:

```text
 eu-region  10:00:00.120  UPDATE users SET email = 'a@example.com' WHERE id = 7;
 us-region  10:00:00.180  UPDATE users SET email = 'b@example.com' WHERE id = 7;
            ... each change replicates to the other region ...
 Which email wins? Both regions must end with the same answer.
```

The usual answers:

- **Last write wins (LWW):** keep the later timestamp. DynamoDB global tables use it, and so does Azure
  Cosmos DB by default. It silently drops the other write, and "later" depends on clocks that are never
  perfectly in sync ([clocks and ordering](/posts/ids-clocks-and-ordering)).
- **A home region per record:** each user or tenant "lives" in one region, which receives all its
  writes. One writer per record means no conflicts. This is the most common practical design, and it
  helps with data-residency rules. On failover, the home moves.
- **Conflict-free data:** append-only logs, sets, counters and CRDTs merge concurrent changes
  automatically ([collaborative editing](/posts/collaborative-editing-ot-crdt)).
- **Synchronous consensus** (Spanner, CockroachDB): no conflicts, slower writes.

Watch rules that span records, like unique usernames or "never sell more seats than exist": with
async replication, two regions can each sell the last seat. Route such writes to one region or a
strongly consistent store, and use IDs that cannot collide (UUIDs, or a region number inside the ID).
Theory: [CAP and consistency models](/posts/cap-theorem-and-consistency-models).

## Hidden single points of failure

Two perfect regions can still fail together because of something both depend on:

- **DNS provider.** In October 2016, a large DDoS attack on the DNS provider Dyn made many well-known
  websites unreachable for hours. Some companies now use two DNS providers.
- **Identity provider.** If engineers reach the cloud console and servers only through single sign-on,
  and it is down, nobody can fail over. Keep tested **break-glass accounts**: emergency credentials
  stored safely and used only in emergencies.
- **Cloud control planes.** The *control plane* is the API that creates and changes resources; the
  *data plane* serves traffic from resources that already exist. Control planes can be impaired or
  flooded during large events. AWS's resilience guidance recommends that recovery rely on the data
  plane, which favours warm standby over pilot light for strict RTOs.
- **CI/CD and artifacts.** A pipeline or container registry only in the failed region means no fixes
  and no new servers ([CI/CD](/posts/ci-cd-and-hotfixes)).
- **Secrets and keys.** Keys only in the dead region mean the standby cannot start and encrypted
  backups cannot be read ([secrets management](/posts/secrets-management)).
- **Monitoring and global config.** Monitoring in the failed region goes dark when you need it. A bad
  config pushed to every region at once breaks them all; roll out region by region.

Meta has described how, in its October 2021 outage, a network maintenance mistake disconnected its
data centers and the internal tools needed for the fix became unreachable too, so engineers had to go
to data centers in person. Recovery paths must work when everything else is broken.

## Failover drills: untested means unknown

A DR plan that has never run is a guess. A typical progression:

1. **Tabletop exercise.** Walk through the runbook on paper. It finds missing permissions and unclear
   owners.
2. **Component drills.** Promote the cross-region replica in staging, restore a backup in the recovery
   region, rebuild the stack from code. Time each step.
3. **Full failover in production.** Move real traffic out of a healthy region in a planned window.
4. **Failback.** Returning is its own procedure: the old primary may hold unreplicated writes, and
   replication must be rebuilt in the other direction.

Compare measured times with your RTO and fix the runbook after every drill. Netflix has described
"Chaos Kong" exercises that simulate losing an entire AWS region, and Google has written about DiRT,
its disaster recovery testing programme. See also [incident
response](/posts/incident-response-and-postmortems).

Automate the *steps*, but think hard before automating the *decision* to leave a region. GitHub's 2018
post-incident analysis describes how a network interruption of under a minute triggered an automatic
database failover to its other US coast. Writes then existed on both sides, and the site was degraded
for about a day while data was reconciled.

## When single-region multi-AZ is enough

Often. Region-wide outages are uncommon compared with failures we cause ourselves: Google's SRE book
estimates that roughly 70% of outages are due to changes in a live system, and a second region does
not help against a bad deploy pushed to both.

One region with several zones is usually enough when the business accepts a few hours of downtime in
a rare regional disaster, no contract or regulator demands more, and the team is small (every region
multiplies what you operate and test). Consider warm standby or active-active when downtime costs more
than a second stack, when it is required, or when you serve several continents anyway.

The baseline everyone should have is cheap: multi-AZ, backups copied to another region and account,
infrastructure as code, and a **measured** restore time.

## Common mistakes

- Replicating the database but forgetting uploads, secrets or keys.
- Assuming DNS failover is instant.
- A standby nobody uses that drifts: old versions, missing config, expired certificates.
- No capacity or quota in the recovery region for full load.
- Last-write-wins on data where a lost write matters.

## Checklist

- [ ] RPO and RTO agreed with the business, per service.
- [ ] At least two zones; backups in another region and account.
- [ ] Replication lag alerted on, using a heartbeat.
- [ ] Secrets, keys, images and break-glass access usable without the primary region.
- [ ] A runbook, regular drills, and RTO/RPO measured in the last one.

## Further reading

- AWS whitepaper: [Disaster Recovery of Workloads on AWS: Recovery in the Cloud](https://docs.aws.amazon.com/whitepapers/latest/disaster-recovery-workloads-on-aws/disaster-recovery-workloads-on-aws.html)
- Google Cloud: [Disaster recovery planning guide](https://cloud.google.com/architecture/dr-scenarios-planning-guide)
- GitHub Engineering (2018): [October 21 post-incident analysis](https://github.blog/2018-10-30-oct21-post-incident-analysis/)
- Meta Engineering (2021): [More details about the October 4 outage](https://engineering.fb.com/2021/10/05/networking-traffic/outage-details/)
- Google SRE Book: [Introduction](https://sre.google/sre-book/introduction/) (see "Change Management")
- Martin Kleppmann, *Designing Data-Intensive Applications*, chapter 5 (multi-leader replication)
