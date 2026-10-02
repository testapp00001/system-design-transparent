+++
title = "ORMs, the N+1 query problem, and when to write SQL yourself"
summary = "What an ORM does for you, how an innocent loop turns into hundreds of queries, how to find and fix N+1 with eager loading and batching, and when plain SQL is the better tool."
tags = ["database","performance","backend"]
level = "beginner"
date = 2026-10-02
+++

You build a page that lists the 50 newest blog posts, each with its author's name. On your laptop it
loads instantly. In production it is slow, and the database graphs show thousands of tiny queries per
second. No single query is slow. There are simply too many of them. The code looks innocent: one loop
and one attribute access. This is the **N+1 query problem**, and it is one of the most common
performance bugs in applications that use an ORM. This article shows how to find and fix it, and
when to skip the ORM and write SQL yourself.

## What an ORM gives you

An **ORM** (object-relational mapper) is a library that maps database tables to classes and rows to
objects. Django's ORM, Rails' Active Record, SQLAlchemy (Python) and Hibernate (Java) are
well-known examples. They give you real benefits:

- **Less repetitive code:** `Post.objects.filter(published=True)` instead of hand-built SQL strings.
- **Safe parameters by default**, which protects you from SQL injection.
- **Relationships as attributes:** `post.author` gives you an `Author` object, without writing a join.
- **Migrations and validations**, built in (Django, Rails) or as a companion tool (Alembic for
  SQLAlchemy migrations).

The cost is that the ORM **hides the SQL**, and with it the number of **round trips** (one request
from your application to the database and one response back). Every round trip costs network time,
even when the database answers in microseconds.

Django, Rails and SQLAlchemy load related objects **lazily** by default: `post.author` is not fetched
until you first touch it. You never load data you don't use, but reading an attribute now *looks*
like reading memory while it may actually be a query.

## The N+1 problem, step by step

Take two tables, `posts` and `authors`. Each post has an `author_id`. In Django:

```python
posts = Post.objects.order_by("-created_at")[:50]   # nothing runs yet

for post in posts:                                  # query 1: load 50 posts
    print(post.title, post.author.name)             # queries 2..51: one per post
```

Turn on the SQL log and you see this (simplified):

```sql
SELECT id, title, author_id, created_at FROM posts ORDER BY created_at DESC LIMIT 50;
SELECT id, name FROM authors WHERE id = 7;
SELECT id, name FROM authors WHERE id = 3;
SELECT id, name FROM authors WHERE id = 7;   -- the same author again
-- ... 47 more
```

That is **1** query for the list plus **N** queries for the authors, where N is the number of posts.
Hence the name. The fix, explained below, needs only two queries:

```text
  N+1: 51 round trips                         Eager loading: 2 round trips

  app                      database           app                      database
   |--- SELECT posts ------->|                 |--- SELECT posts ------->|
   |<-------- 50 rows -------|                 |<-------- 50 rows -------|
   |--- SELECT author 7 ---->|                 |--- SELECT authors ----->|
   |<-------- 1 row ---------|                 |    WHERE id IN (...)    |
   |--- SELECT author 3 ---->|                 |<-------- 31 rows -------|
   |<-------- 1 row ---------|                 |
   |   ... 48 more trips     |                 |   page renders
   |                         |
   |   page renders          |
```

(31 rows, not 50, because several posts share an author.)

Why it hurts:

- **Latency adds up.** Say one round trip costs 1 ms. Then 51 queries spend about 50 ms just waiting
  on the network, against about 2 ms for the fixed version.
- **It grows with your data.** Nesting multiplies it: if each post also shows 10 comments with their
  authors, you get 1 + 50 + 50 + 500 = **601** queries.
- **It hides in development.** With 5 rows and a database on `localhost`, every query looks free.
  The slow query log misses it too, because each single query is fast.
- **It holds database connections longer**, which can drain the
  [connection pool](/posts/connection-pooling).

## Detecting N+1 queries

The fingerprint is always the same: **one query shape repeated many times in a single request**, with
only the id changing.

1. **Read the SQL log in development.** Rails prints every query to the development log by default.
   In Django, set the `django.db.backends` logger to `DEBUG` (it only logs when `DEBUG = True`). In
   SQLAlchemy, pass `echo=True` to `create_engine()`.
2. **Count queries per request.** Log the query count and database time for each request. If a list
   endpoint's query count grows with the page size, you have N+1.
3. **Use a tool that flags it.**
   - **Bullet** is a Ruby gem that watches queries in development. It warns about N+1 queries and
     about eager loading you don't actually use.
   - **Django Debug Toolbar** shows every SQL query for a page and points out similar and duplicated
     queries.
   - **[Tracing](/posts/observability-logs-metrics-traces)** shows an N+1 request as a long
     staircase of short, identical database spans.
4. **Assert it in tests.** Django's `assertNumQueries(n)` fails a test when the number of queries
   is not `n`. Create 20 rows, not 2, so that one query per row is easy to spot.
5. **Look at the database side.** PostgreSQL's `pg_stat_statements` extension groups queries by
   shape. One shape with a huge call count and a tiny average time is a classic N+1 sign.

## Fixing it: eager loading

**Eager loading** means telling the ORM up front which relationships you will use, so it loads them
in bulk *before* the loop starts. There are two ways to do this in SQL.

**Option 1: one query with a JOIN.**

```sql
SELECT posts.*, authors.*
FROM posts
LEFT JOIN authors ON authors.id = posts.author_id
ORDER BY posts.created_at DESC
LIMIT 50;
```

**Option 2: a second query with `IN`.**

```sql
SELECT * FROM posts ORDER BY created_at DESC LIMIT 50;
-- the ORM collects the distinct author_ids from those rows, then:
SELECT * FROM authors WHERE id IN (7, 3, 12, 41);
```

The ORM then attaches each author to its posts in memory.

| | JOIN (one query) | Separate `IN` query |
|---|---|---|
| Round trips | 1 | 1 for the parents, plus 1 per relationship |
| Best for | **To-one** links: post → author, order → customer | **To-many** links: post → comments, user → roles |
| Main risk | Parent columns repeat for every child row. Two joined collections multiply rows (10 comments × 5 tags = 50 rows per post). | Very long `IN` lists for thousands of parents |

Rule of thumb: **JOIN for to-one, a separate `IN` query for to-many.**

The real APIs:

| ORM | JOIN | Separate `IN` query | Chooses for you |
|---|---|---|---|
| Django | `select_related()` (foreign key and one-to-one only) | `prefetch_related()` | – |
| Rails (Active Record) | `eager_load()` | `preload()` | `includes()` |
| SQLAlchemy | `joinedload()` | `selectinload()` | – |

Rails' `includes` uses separate queries by default. It switches to a JOIN when a hash condition
refers to the included table, for example `where(comments: { approved: true })`. If the condition is
an SQL string, add `references(:comments)` to get the same switch.

The same page, with authors, comments and the comments' authors, in each ORM:

```python
# Django
posts = (
    Post.objects
    .select_related("author")                 # JOIN: post -> author
    .prefetch_related("comments__author")     # IN queries: comments, then their authors
    .order_by("-created_at")[:50]
)
# 3 queries in total (posts with authors, comments, comment authors),
# however many posts and comments there are
```

```ruby
# Rails
posts = Post.includes(:author, comments: :author)
            .order(created_at: :desc)
            .limit(50)
# a small, fixed number of queries, however many posts there are
```

```python
# SQLAlchemy 2.0 style
from sqlalchemy import select
from sqlalchemy.orm import joinedload, selectinload

stmt = (
    select(Post)
    .options(
        joinedload(Post.author),                                 # JOIN
        selectinload(Post.comments).joinedload(Comment.author),  # IN query, with a JOIN inside
    )
    .order_by(Post.created_at.desc())
    .limit(50)
)
posts = session.scalars(stmt).all()
```

> [!TIP]
> Make lazy loading fail loudly, so N+1 cannot quietly come back. Rails has `strict_loading`, which
> by default raises an error when a lazy load happens. SQLAlchemy has the `raiseload()` option and
> `lazy="raise"` on a relationship. In Django, query-count tests play a similar role.

## GraphQL: batching with DataLoader

GraphQL makes N+1 almost automatic. Each field has a **resolver**, a small function that loads that
one field. A query for 50 posts with `author { name }` calls the `author` resolver 50 times, once per
post (see [REST, gRPC and GraphQL](/posts/rest-grpc-graphql)).

The standard fix is **batching** with a DataLoader, named after the JavaScript library
`graphql/dataloader`. Resolvers call `loader.load(id)` instead of querying. DataLoader collects all
the ids requested during the same tick of the event loop, then calls your **batch function** once
with all of them:

```js
const DataLoader = require("dataloader");

// create new loaders for every request
function makeLoaders(pool) {
  return {
    author: new DataLoader(async (ids) => {
      const { rows } = await pool.query("SELECT * FROM authors WHERE id = ANY($1)", [ids]);
      const byId = new Map(rows.map((row) => [row.id, row]));
      return ids.map((id) => byId.get(id) ?? null); // same length and order as ids
    }),
  };
}

const resolvers = {
  Post: {
    // called once per post; calls in the same tick become one query
    author: (post, _args, ctx) => ctx.loaders.author.load(post.author_id),
  },
};
```

Two rules matter. The batch function must return one result per key, **in the same order** as the
keys. And loaders should live for **one request**, because they also cache results. Other languages
have the same pattern, such as GraphQL-Ruby's `GraphQL::Dataloader`.

## When to write SQL yourself

ORMs are great at loading and saving a few objects at a time. They are weaker at **set-based** work:
one statement that reads or changes many rows. There, plain SQL is usually clearer and much faster.

**Reports and aggregations.** Don't load 200,000 orders into your app to add them up:

```sql
SELECT created_at::date AS order_date,
       count(*)         AS orders,
       sum(total_cents) AS revenue_cents
FROM orders
WHERE created_at >= now() - interval '30 days'
GROUP BY order_date
ORDER BY order_date;
```

**Bulk updates.** Calling `save()` on each loaded row means one `UPDATE` per row. One statement is
enough:

```sql
UPDATE subscriptions SET status = 'expired'
WHERE status = 'active' AND ends_at < now();
```

ORMs expose this too (Django's `QuerySet.update()`, Rails' `update_all`), but these skip
per-object code: Django's `update()` does not call `save()` or send the save signals, and Rails'
`update_all` skips callbacks and validations. Neither one sets an `updated_at` timestamp for you.
For millions of rows, update in batches (for example by id range) so each transaction stays short.

**Upserts.** "Insert this row, or update it if it already exists." Finding first and then inserting
has a race: two requests both see "not found" and both insert. You get a duplicate row, or, if
there is a unique constraint, an error for one of the requests. The database can do it in one atomic
statement, using a unique constraint on the conflict columns:

```sql
INSERT INTO stock (sku, warehouse_id, quantity)
VALUES ($1, $2, $3)
ON CONFLICT (sku, warehouse_id)
DO UPDATE SET quantity = EXCLUDED.quantity;
```

That is PostgreSQL. MySQL uses `INSERT ... ON DUPLICATE KEY UPDATE`. Many ORMs now wrap this as well,
for example Rails' `upsert_all` and Django's `bulk_create()` with `update_conflicts=True`
(Django 4.1 and later).

**Window functions.** "The 3 latest orders for each customer", rankings and running totals. A window
function computes a value across a group of rows without collapsing them like `GROUP BY` does:

```sql
SELECT customer_id, id, total_cents, created_at
FROM (
    SELECT o.*,
           row_number() OVER (PARTITION BY customer_id ORDER BY created_at DESC) AS rn
    FROM orders o
) ranked
WHERE rn <= 3;
```

Without it, people often write one query per customer: N+1 again.

Mature ORMs have an escape hatch: Django's `Model.objects.raw()` and `connection.cursor()`, Rails'
`find_by_sql`, and SQLAlchemy's `text()`. Most teams end up with a mix: the ORM for everyday work,
SQL for the heavy queries, checked with `EXPLAIN` (see [indexes and EXPLAIN](/posts/database-indexes-and-explain)).

> [!WARNING]
> Raw SQL must still use **parameters** (`$1`, `?`, `%s` or `:name`, depending on the driver). Never
> build SQL by pasting user input into a string. That brings back the SQL injection the ORM was
> protecting you from.

## Other ORM traps

### Loading whole tables

`for user in User.objects.all():` loads every row and column into memory: fine with 1,000 users, a
crash with 10 million. Instead:

- **Iterate in batches:** Rails' `find_each`, Django's `.iterator()`, SQLAlchemy's `yield_per`.
- **Select only the columns you need:** `.only()` or `.values_list()` in Django, `pluck` in Rails.
- **Ask the database for answers:** in Django, `.count()` and `.exists()` run one small query;
  `len(queryset)` loads every row first.
- **Paginate lists** (see [pagination](/posts/api-design-pagination-versioning)).

### Lazy loading in templates and serializers

The N+1 is often hidden in a template (`{{ post.author.name }}` inside a loop), a nested serializer
or a helper. Someone adds one field to the template and every row now runs a query, while the view
code looks unchanged. Decide what data the page needs **in the view** and eager-load it there.

Counting in a loop is the same trap: `post.comments.count()` runs one `COUNT(*)` per post. Use one
grouped query (Django's `annotate(Count("comments"))`) or a stored counter (Rails' `counter_cache`).

### Transactions around network calls

```python
with transaction.atomic():
    order = Order.objects.select_for_update().get(id=order_id)  # locks the row
    payments.charge(order.total)                                # HTTP call: 200 ms, or 30 s
    order.status = "paid"
    order.save()
```

While it waits for the payment provider, this transaction holds a database connection and a row
lock. If the provider slows down, workers pile up here, the pool runs dry, and unrelated pages fail
too. And if the transaction rolls back *after* the charge succeeded, the customer paid but your
database says they didn't.

Keep transactions **short and database-only**. Save a "pending" state in one short transaction,
call the provider outside it (with an [idempotency key](/posts/retries-timeouts-and-idempotency)),
then record the result in a second short transaction. Enqueue jobs and emails *after* commit
(`transaction.on_commit()` in Django, `after_commit` in Rails, or an
[outbox](/posts/distributed-transactions-saga-outbox)); otherwise a worker may look for a row that
is not committed yet. Also watch for settings that wrap every request in a transaction, such as
Django's `ATOMIC_REQUESTS`.

## In practice: a checklist

- [ ] SQL log and an N+1 detector in development; queries per request in production metrics.
- [ ] A test per list endpoint: the query count does not grow with the number of rows.
- [ ] JOIN for to-one, a separate `IN` query for to-many; strict loading where available.
- [ ] GraphQL resolvers use a DataLoader, with new loaders per request.
- [ ] No network calls inside database transactions.

## Common mistakes

- **Eager loading everything "just in case".** You load data the page never shows.
- **Filtering after prefetching.** In Django, `post.comments.filter(approved=True)` ignores the
  prefetched comments and runs a new query per post (in Rails, `post.comments.where(...)` does the
  same). Put the filter into the prefetch, for example with Django's
  `Prefetch("comments", queryset=...)`, or filter the loaded list in code.
- **Hiding N+1 behind a [cache](/posts/caching-strategies).** The page is fast until the cache is
  empty. Fix the queries first.
- **Forgetting the index.** The `IN` query on `comments.post_id` needs an index on that column.
  Django and Rails migrations usually add one for foreign keys, but PostgreSQL itself does not
  create an index on the referencing column (MySQL's InnoDB engine does).

## Further reading

- Django documentation: [Database access optimization](https://docs.djangoproject.com/en/stable/topics/db/optimization/)
- Rails Guides: [Active Record Query Interface](https://guides.rubyonrails.org/active_record_querying.html)
- SQLAlchemy documentation: [Relationship Loading Techniques](https://docs.sqlalchemy.org/en/20/orm/queryguide/relationships.html)
- [Bullet](https://github.com/flyerhzm/bullet): N+1 query detection for Rails
- [graphql/dataloader](https://github.com/graphql/dataloader): the original DataLoader library
- PostgreSQL documentation: [Window Functions tutorial](https://www.postgresql.org/docs/current/tutorial-window.html)
