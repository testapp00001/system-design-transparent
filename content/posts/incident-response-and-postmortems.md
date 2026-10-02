+++
title = "Incident response and blameless postmortems"
summary = "What to do when production breaks: declaring an incident, severity levels, clear roles, fixing the symptoms before the cause, keeping people informed, and writing blameless postmortems whose action items actually get done."
tags = ["reliability", "observability", "devops"]
level = "beginner"
date = 2026-10-02
+++

It is 16:40 on a Friday. An alert fires: 30% of checkouts are failing. Within minutes, six engineers
are in a chat channel. Two of them restart different servers at the same time. Someone starts reading
code to find the bug. Nobody tells the support team, who are answering angry customers with no
information. An hour later the errors stop, and nobody is sure why.

This is an incident without a process. The technical problem is only half of the trouble; the other
half is coordination. This article explains the lightweight process many teams use, and how to learn
afterwards with a **blameless postmortem**.

## What counts as an incident

An **incident** is an unplanned event that hurts users or the business (or is about to) and needs a
coordinated response *now*: the site or a key feature is down or very slow, data is lost or exposed,
payments or sign-ups drop sharply, or something will break soon if nobody acts (a disk almost full, a
TLS certificate that expires tonight). A bug that can wait for the next sprint is not an incident;
it is a ticket.

When in doubt, **declare it**. Closing a small incident costs almost nothing; starting to organise two
hours into a growing outage costs a lot. The Google SRE book suggests a simple test: it is an incident
if you need a second team, if customers can see the problem, or if it is unsolved after an hour of
focused work.

## Severity levels

A **severity level** (SEV1, SEV2, and so on) is a short label for how bad the impact is. It decides who
gets **paged** (alerted by phone, even at night), how often you send updates, and whether a postmortem
is required. Every company defines its own. A typical example:

| Level | Impact | Example | Response |
|---|---|---|---|
| SEV1 | Core product unusable for most users, or data lost or leaked | Checkout down for everyone | Page now, all hands, status page, postmortem |
| SEV2 | An important feature is broken for many users | Search fails for 20% of users | Page now, status page, postmortem |
| SEV3 | Limited impact, a workaround exists | CSV export is slow | Working hours |
| SEV4 | Cosmetic or internal only | An admin chart is broken | Normal ticket |

Define levels by **user impact**, not by cause: "database CPU at 90%" is not a severity, "checkout
fails" is. If unsure, pick the higher level; do not debate it while users are affected.

## Roles: who does what

The most useful idea here comes from emergency services: the **Incident Command System**, developed by
US firefighters. The Google SRE book's chapter "Managing Incidents" describes how Google adapted it for
software. The key idea: **separate the roles, and put one person in charge**.

```text
                      +-------------------------------+
                      |       Incident commander      |
                      |    coordinates and decides;   |
                      |         does not debug        |
                      +---------------+---------------+
                                      |
           +--------------------------+--------------------------+
           |                          |                          |
+----------+----------+    +----------+----------+    +----------+----------+
| Operations lead     |    | Comms lead          |    | Scribe              |
| hands on the        |    | status page,        |    | writes the          |
| system: roll back,  |    | support, managers,  |    | timeline with       |
| debug, fail over    |    | other teams         |    | timestamps          |
+---------------------+    +---------------------+    +---------------------+
```

- **Incident commander (IC)**: sets the severity, decides what to try next, assigns tasks and calls for
  help. The IC does *not* debug; their job is to keep the big picture.
- **Operations lead**: works on the system. Only people the IC assigns change production, so two
  people never restart the same thing at once.
- **Communications lead**: updates the status page, support, managers and other teams, so the people
  fixing things are not interrupted.
- **Scribe**: records what happens. In small incidents the IC or comms lead does this too.

In a small team, the first responder holds every role until a second person joins. Then split: one
coordinates, the other fixes. Hand over roles out loud: "Ana, you are now IC."

## First priority: stop the bleeding

Engineers naturally want to know *why* something broke. Resist this at first. The first goal is
**mitigation**: reducing the harm to users as fast as possible, even before you understand the cause.
The SRE book's chapter "Effective Troubleshooting" gives the same advice.

Most incidents start with a change: a deploy, a config edit, a feature flag, a traffic spike, an expired
certificate. So ask **"what changed?"**, then choose the fastest safe action:

| Mitigation | Helps when | Watch out for |
|---|---|---|
| **Roll back** the last deploy | It started after a release | Database migrations must work with the old code |
| **Turn off a feature flag** | The new code is behind a flag | Only covers flagged code |
| **Fail over** to a replica or region | One database or zone is unhealthy | Replication lag can lose recent writes |
| **Add capacity** | Real load is too high | Does not fix a bug that burns CPU |
| **Shed load** (rate limit, block bad traffic) | Overload or one abusive client | Some users get errors on purpose |
| **Restart** a bad instance | One instance misbehaves | Destroys evidence; save logs first |

Details are in other articles: [rollbacks and hotfixes](/posts/ci-cd-and-hotfixes),
[feature flags](/posts/feature-flags), [failover](/posts/replication-and-high-availability),
[rate limiting](/posts/rate-limiting) and [load shedding](/posts/resilience-patterns).

> [!TIP]
> If a deploy went out shortly before the problem started, roll it back first and ask questions
> later. An unnecessary rollback costs minutes; a slow diagnosis while users suffer costs hours.

Announce each action *before* you take it, and change one thing at a time when you can, or you will
not know which change helped.

## Communication: inside and outside

If everyone asks the people fixing the problem what is happening, nobody fixes anything. Questions
go to the comms lead instead.

**Inside the company**, open **one channel per incident** (for example `#inc-checkout-errors`) and
post updates at a **fixed rhythm**, for example every 30 minutes for a SEV1, even when nothing has
changed. "Still investigating, next update at 17:30" tells people the process is working.

**Outside**, use a **status page**: a public web page that shows whether your services work and what
you are doing about problems. A simple update:

```text
[Investigating] 16:52 UTC
Some customers see errors at checkout. Failed payments are not charged.
We are working on a fix. Next update by 17:20 UTC.
```

A good update says **what users experience** (not internal details), **what they should do**, and
**when the next update will come**. Then keep that promise.

> [!WARNING]
> Host your status page on infrastructure your product does not depend on. In its public summary of
> the February 2017 Amazon S3 outage, AWS explained that for part of the event it could not update its
> own Service Health Dashboard, because the dashboard's admin console depended on S3.

## Keep a timeline

A **timeline** is a list of timestamped facts: what people saw, did and decided. It helps new
responders catch up, stops people repeating what was already tried, and becomes the backbone of the
postmortem.

```text
16:38 UTC  Deploy v142 finishes (new "order history" query)
16:40      Alert: checkout error rate 31% (normal: under 1%)
16:43      Maya declares SEV2 and is IC. Tom is ops lead
16:46      Database CPU at 100%. Slow query log shows the new query
16:49      Decision: roll back to v141 (fastest safe option)
16:55      Rollback complete. Error rate falling
17:10      Error rate normal for 15 minutes. Status page: "monitoring"
17:40      Incident resolved. Postmortem owner: Tom
```

Use **UTC** everywhere, record *why* you made each decision, and paste graph screenshots into the
channel as you go: detailed monitoring data is often summarised or deleted after a while.

## Ending an incident

Two moments are easy to mix up. **Mitigated** means users are no longer affected, but the fix may be
temporary (a rollback, a disabled flag). **Resolved** means the system is stable and you are confident
the problem will not return soon. Before the IC declares it resolved:

- [ ] Errors, latency and business numbers have been normal for a period the team trusts.
- [ ] The status page has a final update saying what happened.
- [ ] Temporary changes (a flag left off, extra servers, a manual data fix) have follow-up tickets.
- [ ] Someone owns the postmortem, with a date.

Then the IC announces the end, so nobody stays half-alert all evening.

## Blameless postmortems

A **postmortem** (or post-incident review) is a written document about an incident: what happened,
why, and what will change. **Blameless** means it looks at the system and the process, not for a person
to punish.

People make mistakes in every system. If the person who ran the wrong command is punished, the next
person hides their mistake, and you lose the information you need to improve. Better questions are
"why did this action make sense at the time?" and "why could one action cause so much damage?" John
Allspaw described this approach at Etsy in his 2012 article "Blameless PostMortems and a Just Culture",
and the SRE book's chapter "Postmortem Culture: Learning from Failure" describes Google's version.

Typical triggers: user-visible downtime above a threshold, any data loss, a manual intervention such
as a rollback, or a problem found by a user instead of by monitoring. A simple template:

```markdown
# Postmortem: checkout errors on 2026-10-02 (SEV2)       Owner: Tom
## Summary               Two or three sentences.
## Impact                Who was affected, how much, for how long.
## Timeline              From the incident, in UTC.
## Detection             How did we find out? How long did it take?
## Contributing factors
## What went well / what went badly / where we got lucky
## Action items          | Action | Type | Owner | Ticket | Due date |
```

"Where we got lucky" appears in the SRE book's example postmortem. Luck is a risk you have not fixed
yet.

### Contributing factors, not one root cause

It is tempting to look for **the** root cause, but serious incidents usually need several things to go
wrong together. In the example above:

1. The new query had no supporting index (see [database indexes](/posts/database-indexes-and-explain)).
2. Tests ran against a tiny database, so the query looked fast.
3. There was no alert on slow queries, so finding the database took six minutes.
4. The rollback was fast because the previous version was one command away (this went well).

Fixing item 1 prevents this exact incident. Fixing items 2 and 3 protects you from a whole family of
future ones. Richard Cook's short paper "How Complex Systems Fail" argues this strongly: failures need
several causes together, so a single "root cause" gives a false picture.

### The limits of "5 whys"

"5 whys" is a technique from manufacturing, associated with Toyota: ask "why?" repeatedly until you
reach a cause. It is simple, but watch what happens:

```text
Why did checkout fail?   The database CPU was at 100%.
Why?                     A new query read the whole orders table.
Why?                     It had no index.
Why?                     The developer did not add one.
Why?                     The developer did not check the query plan.
                         => "Root cause: human error"     (not useful)
```

It follows **one chain**, but incidents have several causes that interact. Where you stop is
arbitrary, and it often stops at a **person**, which quietly brings blame back. Add "how" questions:
"How did this query reach production without anyone seeing its plan?" If an analysis ends at "human
error", treat that as the start of the investigation, not the end.

### Action items that actually get done

A postmortem without completed action items is a story, not an improvement.

| Weak | Strong |
|---|---|
| "Be more careful with queries" | "Fill the staging database with production-sized data" |
| "Improve monitoring" | "Alert when p99 query time is above 500 ms for 5 minutes" |
| 20 items nobody owns | 3 to 5 items, each with one owner, a ticket and a due date |

Give each item **one owner** (a person, not a team) and a ticket in your normal tracker. Mix types:
**prevent** (stop it happening), **detect** (notice faster), **mitigate** (hurt less). Review open
items regularly, and share the postmortem widely: other teams have the same weak spots.

## On-call health

All of this depends on the people who get paged. **On-call** means being responsible for answering
alerts, including at night, for a period such as a week. A tired, overloaded engineer makes worse
decisions, and eventually leaves.

- **Every page needs action.** If the right response is "ignore it", fix or delete the alert. Alert on
  user symptoms (errors, latency); see [observability](/posts/observability-logs-metrics-traces).
- **Limit the load.** The SRE book's chapter "Being On-Call" explains that an incident plus its
  follow-up work takes hours, and sets a target of at most about two incidents per 12-hour shift.
- **Runbooks** (short how-to pages for each alert) let anyone respond, not only the expert.
- **Recovery**: time off after a bad night. Escalating early is good judgement, not weakness.

## Learning from public postmortems

Reading other companies' published postmortems is a cheap way to learn how real systems fail:

- **GitLab, January 2017.** While fixing a replication problem, an engineer deleted the data directory
  on the primary database server instead of the secondary. Several backup methods turned out not to be
  working, and GitLab lost roughly six hours of database data. Lesson: a backup you have never restored
  is not a backup (see [database backups and recovery](/posts/database-backups-and-recovery)).
- **Amazon S3, February 2017.** AWS reported that a command meant to remove a small number of servers
  was entered incorrectly and removed many more. Lesson: tools should limit how much one command can do.
- **Cloudflare, July 2019.** Cloudflare described how a firewall rule with a badly behaving regular
  expression used up CPU across its network, partly because such rule changes went out everywhere at
  once. Lesson: configuration changes need staged rollouts, just like code.

## Common mistakes

- **Debugging before mitigating**, while users wait.
- **Nobody in charge**, or an IC who is also debugging.
- **Silence**: customers learn about the outage from social media.
- **Declaring too late**, because declaring feels like admitting failure.
- **Blame in the postmortem**, even subtle ("Tom should have known"), and action items with no owner.

## Further reading

- Google SRE Book: [Managing Incidents](https://sre.google/sre-book/managing-incidents/),
  [Postmortem Culture: Learning from Failure](https://sre.google/sre-book/postmortem-culture/),
  [Being On-Call](https://sre.google/sre-book/being-on-call/) and the
  [Example Postmortem](https://sre.google/sre-book/example-postmortem/)
- Google SRE Workbook: [Incident Response](https://sre.google/workbook/incident-response/)
- PagerDuty: [Incident Response documentation](https://response.pagerduty.com/)
- GitLab: [Postmortem of database outage of January 31](https://about.gitlab.com/blog/2017/02/10/postmortem-of-database-outage-of-january-31/)
- Richard Cook: [How Complex Systems Fail](https://how.complexsystems.fail/)
- Dan Luu: [a collection of public postmortems](https://github.com/danluu/post-mortems)
