+++
title = "API design that ages well: pagination, versioning, errors and evolution"
summary = "Offset vs cursor (keyset) pagination and why offset gets slow, how to evolve an API without breaking clients, consistent error formats, filtering and naming conventions."
tags = ["api-design", "backend", "performance"]
level = "intermediate"
date = 2026-10-02
+++

An API is a promise. Once other people's code depends on it, every change is a negotiation. The
decisions that are cheapest to get right on day one — pagination, error format, how to evolve — are
the most expensive to change later. This article collects the conventions that age well.

## Pagination: never return unbounded lists

Every endpoint that returns a list will one day be asked for a list with a million items. Paginate
from the start, with a default and a maximum page size.

### Offset pagination

```http
GET /articles?limit=20&offset=40
```

```sql
SELECT * FROM articles ORDER BY published_at DESC LIMIT 20 OFFSET 40;
```

- Simple; supports "jump to page 7" and total counts.
- **Gets slower the deeper you go**: `OFFSET 100000` makes the database read and discard 100,000 rows.
- **Unstable**: if a new article is published while a user pages, everything shifts by one — they see
  an item twice or skip one.

Fine for admin screens and small datasets (this site uses it, with a few dozen articles).

### Cursor (keyset) pagination

Instead of "skip N rows", say "continue after the last item I saw":

```http
GET /articles?limit=20
-> { "data": [...], "next_cursor": "eyJ0IjoiMjAyNi0xMC0wMlQwOTowMDowMFoiLCJpZCI6OTgxfQ" }

GET /articles?limit=20&cursor=eyJ0IjoiMjAyNi0xMC0wMlQwOTowMDowMFoiLCJpZCI6OTgxfQ
```

The cursor encodes the sort key of the last item (here `published_at` and `id`, base64-encoded so
clients treat it as opaque):

```sql
SELECT * FROM articles
WHERE (published_at, id) < ($last_published_at, $last_id)   -- row-value comparison
ORDER BY published_at DESC, id DESC
LIMIT 20;
```

With an index on `(published_at, id)`, page 5,000 is as fast as page 1, and inserts don't cause
duplicates or gaps. Include a unique tie-breaker (`id`) in the sort, or items with equal timestamps
can be skipped.

Trade-offs: no "jump to page 7", and total counts are a separate (often approximate) query. For feeds,
timelines, infinite scroll, sync and exports, keyset pagination is the right default.

## Versioning and evolution

### Prefer evolution over versions

Most changes can be made **backward compatible**, so no new version is needed:

| Safe (additive) | Breaking |
|---|---|
| Add an optional request field | Remove or rename a field |
| Add a response field | Change a field's type or meaning |
| Add a new endpoint | Make an optional field required |
| Add a new enum value (*if clients were told to expect unknown values*) | Change error codes or status codes clients rely on |
| Accept more input formats | Change default behaviour |

The contract that makes this work, on both sides: **clients must ignore unknown fields** (the
"tolerant reader"), and **servers must not remove things clients use**. Document that unknown enum
values may appear and clients should handle them gracefully.

### When you must break things

Pick one style and use it consistently:

- **URL versioning**: `/v1/orders`, `/v2/orders`. Obvious, easy to route and cache. The most common.
- **Header versioning**: `Accept: application/vnd.example.v2+json` or a custom header. Cleaner URLs,
  less visible.
- **Date-based versions** (Stripe's approach): each account is pinned to the API version from when it
  integrated, and the server translates responses between versions internally. Powerful, but needs
  infrastructure.

Then **deprecate deliberately**: announce, add a `Deprecation` and `Sunset` header to responses, log
who still calls the old version, contact them, and only remove it when traffic is (near) zero.

## Errors: one consistent shape

Use HTTP status codes for the category, and a consistent JSON body for details. RFC 9457 ("Problem
Details for HTTP APIs") defines a standard shape:

```http
HTTP/1.1 422 Unprocessable Content
Content-Type: application/problem+json

{
  "type": "https://api.example.com/problems/validation-error",
  "title": "Your request is not valid.",
  "status": 422,
  "detail": "2 fields are invalid.",
  "errors": [
    { "field": "email", "code": "invalid_format", "message": "must be a valid email address" },
    { "field": "quantity", "code": "out_of_range", "message": "must be between 1 and 99" }
  ],
  "request_id": "req_8f2c1a"
}
```

Guidelines:

- **Status codes by category**: `400` malformed, `401` not authenticated, `403` not allowed, `404` not
  found, `409` conflict (duplicate, version mismatch), `422` validation failed, `429` rate limited,
  `5xx` server's fault. Clients and monitoring rely on the category.
- **Machine-readable codes** (`invalid_format`, `card_declined`) that clients can branch on; human
  messages can change, codes cannot.
- **A request id** in every response (and in your logs) so a support ticket can be traced.
- **Never leak internals**: no stack traces or SQL errors in responses.

## Other conventions that save pain

- **Consistent naming**: plural nouns for collections (`/orders`, `/orders/42`), one case style for
  JSON fields (`snake_case` or `camelCase`, never both).
- **Timestamps in UTC, ISO 8601** (`2026-10-02T09:00:00Z`). Money as integer minor units plus currency
  (`{"amount": 1999, "currency": "EUR"}`), never floats.
- **Ids as strings** in JSON — JavaScript can't represent 64-bit integers exactly. Prefixed ids
  (`ord_8f2c...`) make logs and support easier.
- **Filtering and sorting** with explicit, documented parameters: `?status=paid&sort=-created_at`.
  Allow-list sortable/filterable fields (each needs an index).
- **Idempotency** for unsafe operations: accept an `Idempotency-Key` header on `POST` — see
  [retries and idempotency](/posts/retries-timeouts-and-idempotency).
- **Rate limits** with `429` and `Retry-After` — see [rate limiting](/posts/rate-limiting).
- **Partial updates** with `PATCH` and a clear semantics (JSON Merge Patch is the simplest).
- **Long-running operations**: return `202 Accepted` with a status URL to poll, instead of holding the
  request open.
- **A written contract**: OpenAPI (or `.proto`/GraphQL schema), with examples, kept in the repo and
  validated in CI.

## Checklist for a new endpoint

- [ ] Lists are paginated (keyset for large or growing collections), with a max page size.
- [ ] Errors follow the shared format with stable codes and a request id.
- [ ] The change is additive, or it ships as a new version with a deprecation plan.
- [ ] Unsafe operations are idempotent or accept an idempotency key.
- [ ] Every filter and sort option is backed by an index.
- [ ] The OpenAPI spec is updated and examples work.

## Further reading

- RFC 9457: [Problem Details for HTTP APIs](https://www.rfc-editor.org/rfc/rfc9457)
- Markus Winand: [Paging Through Results (keyset pagination)](https://use-the-index-luke.com/no-offset)
- Stripe: [APIs as infrastructure: future-proofing Stripe with versioning](https://stripe.com/blog/api-versioning)
- [Google API Design Guide](https://cloud.google.com/apis/design) and [Microsoft REST API Guidelines](https://github.com/microsoft/api-guidelines)
- RFC 8594: [The Sunset HTTP Header Field](https://www.rfc-editor.org/rfc/rfc8594)
