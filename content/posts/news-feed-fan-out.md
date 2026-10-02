+++
title = "Design a news feed: fan-out on write vs fan-out on read"
summary = "How a home feed is built at scale: fan-out on read vs fan-out on write, the celebrity problem and the hybrid model, storing ids and hydrating them, cache sizing, cursor pagination, a live 'new posts' banner, and deletes."
tags = ["system-design","scalability","caching"]
level = "intermediate"
date = 2026-10-02
+++

Your app lets people follow each other, and the home screen shows recent posts from everyone you
follow. Version one is one SQL query: join `follows` to `posts`, sort by time, take 20. Then the app
grows, some users follow thousands of accounts, and the feed query becomes the most expensive query
in your database. Every app open runs it again, even when nothing changed.

There are two very different ways to build a home feed (also called a *timeline*), and each is cheap
exactly where the other is expensive. This article covers both, the "celebrity problem", and the
production details. If design exercises are new to you, start with
[how to approach system design](/posts/how-to-approach-system-design).

## Requirements and estimates

What the system must do:

- Users create posts and follow accounts.
- The **home feed** shows posts from followed accounts, newest first, 20 at a time (infinite scroll).
- A banner says "3 new posts" when new content arrives while the feed is open.
- Deleted posts, blocked users and private accounts disappear from feeds quickly.

How well: the feed opens in a few hundred milliseconds. A new post reaches followers within seconds.
Feeds can be *eventually consistent*: two followers may see a post a few seconds apart. Reads
dominate: people scroll much more than they post.

Estimates (practice assumptions, not data from any real company):

```text
registered users           200 million
daily active users (DAU)    50 million
feed opens       10 per DAU per day   -> 500M/day -> ~5,800/s  (peak ~3x: ~17,000/s)
new posts        0.2 per DAU per day  ->  10M/day ->   ~115/s  (peak ~350/s)
accounts followed           200 on average
```

Each follow links one follower to one followee, so the average account also has 200 followers. But
most accounts have few followers, and a few have millions. That unevenness causes the hardest
problem below.

## Fan-out on read (the pull model)

*Fan-out* means one action that turns into many. In the pull model it happens at **read** time: to
build my feed, the server looks up everyone I follow and pulls their recent posts.

```sql
-- newest 20 posts from the accounts that $viewer follows (PostgreSQL)
SELECT p.id, p.author_id, p.created_at
FROM follows f
CROSS JOIN LATERAL (
    SELECT id, author_id, created_at
    FROM posts
    WHERE author_id = f.followee_id
    ORDER BY created_at DESC, id DESC
    LIMIT 20                        -- at most 20 per followed account
) p
WHERE f.follower_id = $viewer
ORDER BY p.created_at DESC, p.id DESC
LIMIT 20;
```

With an index on `posts (author_id, created_at DESC, id DESC)`, this runs one short index scan per
followed account. The `LATERAL` subquery matters: a plain join can make the database read *every*
post of every followed account before sorting (see [indexes and EXPLAIN](/posts/database-indexes-and-explain)).

Pull is simple, posting costs one insert, and deletes, unfollows and privacy changes apply instantly,
because nothing is precomputed. But every feed open repeats the work: at peak, 17,000 feeds/s × 200
followed accounts is about **3.4 million index lookups per second**, mostly returning the same answer
as last time. While your database handles it, pull is the right choice. Many products never outgrow it.

## Fan-out on write (the push model)

The push model moves the work to **write** time. Every user has a precomputed timeline: a list of
post ids, newest first. When someone posts, a background job adds the post id to each follower's
timeline. Reading a feed becomes one lookup.

```text
 author --POST /posts--> [post service] --1. INSERT -------------------> [posts DB]
                               |
                               +--2. enqueue "post 981 by user 7" --> [queue]
                                                                         |
                                                                         v
                                                                 [fan-out workers]
                                 3. read followers of user 7             |
                                 4. add 981 to each follower's timeline  |
                      +-----------------------+--------------------------+
                      v                       v                          v
                 timeline:12             timeline:13      ...      timeline:9001   (Redis)
```

Fan-out runs **asynchronously** through a [queue](/posts/message-queues-and-event-streams): the
author gets a response once the post is saved, and followers see it seconds later. A Redis **sorted
set** (members ordered by a numeric score) fits a timeline well. The member is the post id; the score
is the post time in milliseconds:

```python
MAX_LEN = 500              # keep the newest 500 ids per timeline
BIG_ACCOUNT = 100_000      # follower count; tune from your own data

def fan_out(post_id: int, author_id: int, created_ms: int) -> None:
    if follower_count(author_id) >= BIG_ACCOUNT:
        return  # pulled at read time instead (see the hybrid model)
    for batch in follower_ids_in_batches(author_id, size=1000):
        pipe = redis.pipeline(transaction=False)   # one round trip per batch
        for follower_id in with_cached_timeline(batch):
            key = f"timeline:{follower_id}"
            pipe.zadd(key, {post_id: created_ms})          # adding twice changes nothing
            pipe.zremrangebyrank(key, 0, -(MAX_LEN + 1))   # trim to the newest MAX_LEN
        pipe.execute()
```

- **The insert is idempotent**, so a retried job creates no duplicates (see
  [retries and idempotency](/posts/retries-timeouts-and-idempotency)).
- **Only push into timelines that exist.** Pushing into a missing one creates a timeline with only
  the newest posts: an almost empty feed. Users without a cached timeline (inactive, or evicted) get
  one rebuilt from the pull query when they return.
- A Redis **list** (`LPUSH` + `LTRIM`) uses less memory but keeps insertion order, and workers can
  finish out of order. A sorted set keeps the order by score.

Write cost: 10 million posts × 200 followers = **2 billion timeline inserts per day**, about 23,000
per second. If only active users (1 in 4 here) have a timeline, it is about four times less.

## The celebrity problem and the hybrid model

Now an account with 30 million followers posts. If your workers manage 200,000 inserts per second in
total, that one post takes **150 seconds** to reach everyone, and every other post waits behind it.
Much of the work is wasted: many of those followers will not open the app today.

The usual answer is a **hybrid**: posts from normal accounts are pushed, and posts from very big
accounts are not fanned out. At read time, the feed service fetches the recent posts of the few big
accounts this viewer follows and merges them in.

```text
  timeline:42 (pushed ids)          --+
  recent posts of big account A     --+--> merge by time --> hydrate --> filter --> 20 posts
  recent posts of big account B     --+
```

The pulled part is cheap, because millions of viewers read the *same* "recent posts of account A"
list. It is one hot cache entry, not millions of queries. Keep it in Redis, and in each feed
server's memory for a second or two (see [caching strategies](/posts/caching-strategies)). Pick the
"big" threshold from your data: compare the cost of one fan-out with an extra merge on every read.

Martin Kleppmann's book *Designing Data-Intensive Applications* uses a social network's home timeline
(Twitter's, in the first edition) to explain this trade-off, including the hybrid.

## Store ids, then hydrate

A timeline stores **post ids, not posts**. Turning ids into full posts at read time is called
**hydration**:

```text
1. ids    newest ~25 ids from timeline:42 + big accounts   (a few extra, for filtering)
2. posts  one multi-get from the post cache                 (misses: WHERE id = ANY($ids))
3. drop   deleted posts, blocked authors, posts the viewer may no longer see
4. extras authors, like counts, "did I like this?"          (batched, never one per post)
5. reply  20 posts + next_cursor
```

Why ids? A post might be 1 KB and an id is 8 bytes, and each post is copied to 200 timelines on
average. Edits and deletes become one write, because the post exists once. The price is extra
lookups on every read, so batch them: a loop that fetches posts one by one is the
[N+1 problem](/posts/orm-n-plus-one) with a cache instead of a database.

## Cache sizing

Memory = cached users × entries per timeline × bytes per entry.

| Design | Per user | 50M cached users |
|---|---|---|
| Full posts (~1 KB) in each timeline, 500 entries | ~500 KB | ~25 TB |
| Ids only, raw (8-byte id + 8-byte score), 500 entries | 8 KB | 400 GB |
| Ids, assuming ~64 bytes per entry with Redis overhead | 32 KB | 1.6 TB |

- **The 64 bytes is a planning assumption.** Data structures add pointers and bookkeeping to every
  element. Load a realistic timeline and measure with `MEMORY USAGE timeline:42`. Replicas double it.
- **Who gets a timeline** is the biggest lever. All 200 million registered users instead of the 50
  million active ones would need four times the memory.
- **Length** is the second lever. Measure how deep users really scroll; past the cache, use pull.

The post cache is small in comparison: 10 million new posts a day × 1 KB is about 10 GB per day.

Timelines are **derived data**: they can be rebuilt from `posts` and `follows`, so losing a cache
node means slow feeds, not lost data. Rebuild lazily on first read, but limit concurrent rebuilds so
a cache failure does not become a database stampede.

## Cursor pagination

Never page a timeline with offsets. New posts arrive at the top while the user scrolls, so "skip 20"
shifts every page and the user sees duplicates. Use a cursor, "posts older than the last one I saw"
(see [API pagination](/posts/api-design-pagination-versioning)), holding the time and id of the last
item returned:

```text
# first page: the newest entries (25, because some may be filtered out)
ZRANGE timeline:42 +inf -inf BYSCORE REV LIMIT 0 25 WITHSCORES

# next page: entries at or before the last item's time; skip the ones already returned
ZRANGE timeline:42 1790000123456 -inf BYSCORE REV LIMIT 0 25 WITHSCORES
```

This form of `ZRANGE` needs Redis 6.2 or later. Encode `(time, id)` as an opaque string for the
client; the id breaks ties between posts from the same millisecond. When the cursor passes the oldest
cached entry, switch to the pull query with `WHERE (created_at, id) < ($time, $id)`.

> [!WARNING]
> Redis sorted set scores are 64-bit floating point numbers, which hold integers exactly only up to
> 2^53. Snowflake-style ids are much larger, so using the id as the score silently rounds it. Use a
> millisecond timestamp as the score. See [IDs and ordering](/posts/ids-clocks-and-ordering).

## Chronological or ranked?

A **chronological** feed is predictable and easy to explain, and the stored timeline *is* the feed.

A **ranked** feed orders posts by predicted interest. Each read collects a few hundred candidates
(the stored timeline, big accounts, maybe recommendations), fetches features in batches (post age,
likes, how often the viewer interacts with the author), scores them with a formula or a
machine-learning model, applies rules (hide seen posts, mix authors), and cuts a page. What changes:

- **Pagination.** Scores change between requests, so "older than the cursor" stops working. A common
  approach: rank once when the session starts, store the ordered ids for some minutes, and page
  through that snapshot. Offsets are fine here, because the snapshot does not change.
- **Latency and cost.** Scoring needs data about every candidate, so limit the candidate count.

## A real-time "new posts" indicator

Do not push full posts to an open feed. Push a small hint; the client loads the posts through the
normal API when the user taps it. **Server-Sent Events** (SSE) fit well: one direction, plain
HTTP, and the browser reconnects automatically (see
[polling, SSE and WebSockets](/posts/realtime-polling-sse-websockets)).

```text
GET /feed/events            (Accept: text/event-stream)

event: new_posts
data: {"count": 3}

```

After a fan-out worker updates `timeline:42`, it publishes a tiny message on a per-user channel (for
example with [Redis Pub/Sub](/posts/pub-sub-redis-nats)). The SSE server holding user 42's connection
forwards it, at most one event every few seconds, and the client shows "3 new posts".

Big accounts are not fanned out, so no per-user message exists for their posts. Each SSE server can
subscribe once to a big account's channel and notify its local followers, like the two-level fan-out
for [large chat rooms](/posts/scaling-websockets-chat). If the connection drops, nothing is lost: the
event is only a hint, and the feed is the source of truth.

## Deletes, unfollows and privacy changes

A precomputed timeline stores decisions made at fan-out time: "user 42 may see post 981". Those
decisions can become wrong. One rule keeps you safe: **the timeline is a list of candidates, not a
permission.** Check visibility during hydration, on every read, with current data (see
[authorization models](/posts/authorization-models)).

| Change | Immediately (read path) | Later (background) |
|---|---|---|
| Post deleted | Mark deleted in DB and post cache; hydration drops it | Optional: remove the id from timelines |
| Unfollow, block, mute | Filter against the viewer's current lists | Remove that author's ids from the timeline |
| Account made private | Check the author's current setting against the viewer | None, if checks happen on read |
| Account deleted | Treat all its posts as deleted | Purge timelines, caches, search indexes |
| New follow | Nothing | Backfill the account's recent posts |

Privacy laws in many countries give users a right to erasure, so make sure deletion reaches every
copy: timelines, caches, search indexes and analytics.

## Trade-offs: which one to use

| | Fan-out on read (pull) | Fan-out on write (push) | Hybrid |
|---|---|---|---|
| Cost of a post | 1 insert | 1 insert + 1 per follower | Push, capped for big accounts |
| Cost of a feed read | 1 lookup per followed account | 1 timeline read + hydration | Timeline + a few hot lists |
| Freshness | Immediate | Seconds (fan-out lag) | Seconds |
| Memory | Small | Large | Large |
| Struggles with | Users who follow many accounts | Accounts with many followers | Complexity |

Start with pull and a good index, and move to push only when feed reads dominate your database load.
Once you push, monitor feed latency (p99), timeline cache hit rate, and **fan-out lag**: the time from
"post saved" to "last follower's timeline updated".

## Common mistakes

- **Fanning out inside the `POST` request.** One big account makes the request time out.
- **Storing full posts in timelines.** Memory explodes, and every edit needs another fan-out.
- **Pushing into missing timelines**, which creates feeds with only the newest posts.
- **Offset pagination** on a feed that grows at the top.
- **Trusting the timeline for permissions** instead of checking at read time.

## Further reading

- Martin Kleppmann: [Designing Data-Intensive Applications](https://dataintensive.net/), which uses
  home timelines as a worked example
- Adam Silberstein, Jeff Terrace, Brian F. Cooper and Raghu Ramakrishnan: *Feeding Frenzy: Selectively
  Materializing Users' Event Feeds* (SIGMOD 2010), on choosing push or pull per producer and consumer
- Redis docs: [Sorted sets](https://redis.io/docs/latest/develop/data-types/sorted-sets/) and
  [ZRANGE](https://redis.io/docs/latest/commands/zrange/)
- MDN: [Using server-sent events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events)
- Markus Winand: [Paging Through Results](https://use-the-index-luke.com/no-offset)
