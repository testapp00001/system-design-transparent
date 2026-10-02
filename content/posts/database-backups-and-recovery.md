+++
title = "Database backups and point-in-time recovery: the restore is what matters"
summary = "Logical vs physical backups, continuous WAL/binlog archiving for point-in-time recovery, the 3-2-1 rule, RPO/RTO — and why an untested backup is just a hope."
tags = ["database", "postgresql", "mysql", "reliability", "devops"]
level = "intermediate"
date = 2026-10-02
+++

In January 2017, an engineer at GitLab, working late to fix replication during an incident, ran a
deletion command on the wrong database server and removed hundreds of gigabytes of production data.
When the team turned to their backups, they found that several of their backup mechanisms had not been
working. They recovered from a disk snapshot that happened to have been taken about six hours earlier,
and lost roughly six hours of data. To their great credit, GitLab live-streamed the recovery and
published a detailed postmortem.

An old saying in operations sums up the lesson: **nobody cares about backups; everybody cares about
restores.** This article
explains the kinds of backups, how point-in-time recovery works, and how to know yours actually work.

## What can go wrong (and what each needs)

| Failure | Protects you |
|---|---|
| Disk or server dies | Replication (fast), backups (slower) |
| Accidental `DELETE`/`DROP`, buggy migration, bad deploy | **Point-in-time recovery** — replicas copy the mistake instantly |
| Ransomware / compromised credentials | Backups the attacker can't delete (separate account, immutable storage) |
| Whole region or provider outage | Backups (and replicas) in another region/provider |
| Silent corruption | Backups + regular verification |

Replication is not on the list for logical mistakes: see [replication and HA](/posts/replication-and-high-availability).

## Logical backups

A **logical backup** exports the data as SQL statements or a portable archive:

```sh
pg_dump --format=custom --file=app-2026-10-02.dump appdb      # PostgreSQL
pg_restore --dbname=appdb_restored app-2026-10-02.dump

mysqldump --single-transaction --routines appdb > app.sql       # MySQL (InnoDB, consistent snapshot)
```

- Portable across versions and architectures; can restore a single table.
- Easy to understand and script.
- **Slow** for large databases (hours for hundreds of GB) to create and especially to restore
  (indexes are rebuilt).
- Only as fresh as the last dump: a nightly dump means up to 24 hours of lost data.

Great for small databases, migrations between versions, and extracting data — not sufficient alone
for a large production database.

## Physical backups + continuous archiving

A **physical backup** copies the database's data files. On its own, a copy of files taken while the
database is running would be inconsistent — but combined with the **write-ahead log** (PostgreSQL WAL,
MySQL binlog), it becomes the foundation of **point-in-time recovery (PITR)**:

```text
 base backup (Sunday 02:00)        continuous WAL archive (every change, shipped every few seconds)
 [================]  + [wal][wal][wal][wal][wal][wal][wal][wal][wal][wal] ...
                                                         ^
                                         "restore to Tuesday 14:31:59, one second before the DROP TABLE"
```

To recover: restore the base backup, then **replay** the archived WAL up to any moment you choose.
With WAL shipped continuously, you can lose as little as a few seconds of data — and you can recover to
just *before* a mistake, which no replica can do.

Tools that manage base backups, WAL archiving, retention, compression, encryption and restores:

- **PostgreSQL**: pgBackRest, WAL-G, Barman (and the built-in `pg_basebackup` + `archive_command`).
- **MySQL**: Percona XtraBackup + binlog archiving; MySQL Enterprise Backup.
- **Managed databases** usually provide automated backups with PITR within a retention window — check
  that it's enabled and what the retention is.

Storage snapshots (EBS/LVM/ZFS) can also serve as base backups if the database is prepared for them,
but they typically live with the same provider and account — not a complete strategy on their own.

## RPO and RTO

Two numbers turn "we have backups" into a real requirement:

- **RPO (recovery point objective)**: maximum acceptable data loss. Nightly dumps → RPO 24 h.
  Continuous WAL archiving → RPO of seconds to minutes.
- **RTO (recovery time objective)**: maximum acceptable downtime. Restoring 2 TB from object storage
  and replaying a day of WAL can take many hours — if your RTO is one hour, you need something else
  (replicas, delayed replicas, smaller databases, faster restore infrastructure).

Measure your actual RTO by doing restores. It is almost always longer than people assume.

## The 3-2-1 rule

Keep at least:

- **3** copies of your data,
- on **2** different types of storage,
- with **1** copy off-site (another region, another provider or account).

Modern additions: one copy should be **immutable or offline** (e.g. object storage with object lock,
or a separate account with delete permissions nobody uses day to day), so ransomware or a compromised
admin key can't destroy production *and* its backups.

## Test restores, automatically

A backup you haven't restored is a hypothesis. Make restore tests routine:

1. **Automate a restore** regularly (daily or weekly) into a scratch environment from the real backup
   storage, using the documented procedure.
2. **Verify** it: the database starts, row counts and a few checksums or business queries match
   expectations, the latest restored transaction is as recent as your RPO promises.
3. **Time it**, and compare with your RTO.
4. **Alert** when a backup job or a restore test fails, and when no backup has completed in N hours
   (silence is a failure too).
5. **Practice** a full disaster recovery with people involved a couple of times a year, following the
   runbook. Fix the runbook every time.

## Other things that get forgotten

- **Secrets and config**: can you rebuild the servers, credentials and configuration needed to run the
  restored database?
- **Encryption keys**: encrypted backups are useless without the key — store it separately and safely.
- **Retention and law**: how long must you keep backups — and when must you *delete* data (e.g. user
  deletion requests under privacy laws)? Plan for both.
- **Other data stores**: object storage (user uploads), search indexes (rebuildable?), queues, Redis
  (cache or source of truth?).

## Checklist

- [ ] Continuous archiving with PITR for production databases (or managed PITR enabled).
- [ ] Defined RPO/RTO, and measured restore times that meet them.
- [ ] 3-2-1, with one immutable or separately-credentialed copy.
- [ ] Automated, verified restore tests with alerting.
- [ ] A written, rehearsed recovery runbook.

## Further reading

- GitLab (2017): [Postmortem of database outage of January 31](https://about.gitlab.com/blog/2017/02/10/postmortem-of-database-outage-of-january-31/)
- PostgreSQL docs: [Backup and Restore](https://www.postgresql.org/docs/current/backup.html), especially [Continuous Archiving and PITR](https://www.postgresql.org/docs/current/continuous-archiving.html)
- [pgBackRest user guide](https://pgbackrest.org/user-guide.html)
- MySQL docs: [Point-in-Time Recovery Using the Binary Log](https://dev.mysql.com/doc/refman/8.0/en/point-in-time-recovery.html)
