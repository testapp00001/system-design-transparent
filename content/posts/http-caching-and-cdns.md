+++
title = "HTTP caching and CDNs: Cache-Control, ETags and caching at the edge"
summary = "How browsers and CDNs decide what to store and for how long: Cache-Control directives, ETags and 304 responses, Vary and cache keys, purging, and how to cache aggressively without ever serving one user's data to another."
tags = ["caching","networking","performance"]
level = "intermediate"
date = 2026-10-02
+++

You deploy a fix to your JavaScript bundle, but users report the old bug for hours. Your public
catalogue API returns the same JSON thousands of times per second. Or a support ticket says: "I opened
my account page and saw someone else's name." All three are HTTP caching problems, controlled by a
handful of response headers. This article explains those headers, what CDNs add, and defaults you
can copy. (Caching *inside* your application is covered in [caching strategies](/posts/caching-strategies).)

## Who caches your responses

Your server is the **origin**: the source of truth. Between it and the user there can be several
caches:

```text
 user's browser        CDN edge (PoP)          CDN shield (optional)   origin
+---------------+     +----------------+     +----------------+     +--------------+
| private cache | --> | shared cache   | --> | shared cache   | --> | your servers |
| one user      |     | many users     |     | all edges      |     | (the truth)  |
+---------------+     +----------------+     +----------------+     +--------------+
```

- A **private cache** belongs to one user, like the browser's cache. It may store personal responses.
- A **shared cache** serves many users: a CDN, a caching reverse proxy (nginx, Varnish) or a company
  proxy. Anything it stores may be served to *anyone* who sends a matching request.

A **CDN** (content delivery network) runs many data centres, called **points of presence (PoPs)** or
**edges**, close to users. If a PoP has a usable copy, it answers at once (a **hit**). If not (a
**miss**), it fetches the response from your origin, maybe stores it, and passes it on.

A stored response is **fresh** while its allowed lifetime has not passed; a cache may serve it without
asking the origin. After that it is **stale**, and the cache must normally **revalidate** it: ask the
origin "is my copy still current?"

## Cache-Control: how long, and who may store it

The main header is `Cache-Control`, a comma-separated list of directives such as `max-age=300`.

| Directive | Meaning |
|---|---|
| `max-age=N` | Fresh for N seconds after the origin generated it. Applies to every cache. |
| `s-maxage=N` | Like `max-age`, but only for shared caches, where it wins over `max-age`. Browsers ignore it. |
| `public` | Any cache may store it, even where it normally would not (such as a response to an authenticated request). |
| `private` | Only the browser may store it. Shared caches must not. |
| `no-cache` | Caches may store it, but must revalidate before **every** use. |
| `no-store` | No cache may store it at all. |
| `must-revalidate` | Once stale, never use it without a successful revalidation. |
| `immutable` | Will not change while fresh: do not revalidate, even on reload. Not every browser supports it. |
| `stale-while-revalidate=N` | Once stale, serve it for N more seconds while refreshing it in the background. |
| `stale-if-error=N` | If the origin fails (500, 502, 503, 504 or no answer), serve the stale copy for up to N more seconds. |

> [!WARNING]
> `no-cache` does **not** mean "do not cache". It means "cache, but check with me every time", and
> that check is cheap when nothing changed (see 304 below). The directive that forbids storing is
> `no-store`.

Three details that surprise people:

- **`public` is rarely needed.** `max-age` already makes a response cacheable by shared caches.
  `public` mainly adds permission to cache responses to *authenticated* requests: the dangerous case.
- **Freshness counts from the origin.** A CDN copy of a `max-age=60` response stored 50 seconds ago
  arrives with `Age: 50`, so the browser keeps it for only 10 more seconds.
- **No `Cache-Control` does not mean no caching.** Without an explicit lifetime, caches may guess one,
  for example from `Last-Modified`. The HTTP caching standard, RFC 9111, mentions 10% of the time
  since the last change as a typical value. Always send an explicit `Cache-Control`.

The two `stale-*` directives come from RFC 5861. On a timeline:

```text
Cache-Control: max-age=60, stale-while-revalidate=30, stale-if-error=86400

age (s)  0                     60                      90
         |------ fresh --------|--- stale but usable --|----- stale -------------------->
         served from cache,    served at once; cache   must wait for the origin; if the
         no origin request     refreshes it in the     origin is failing, the old copy
                               background              may be served for up to one day
```

Inside the `stale-while-revalidate` window, users do not wait for a refresh, and `stale-if-error`
can hide short origin outages. Many CDNs and caching proxies implement both directives; browser
support is partial.

## Validation: ETag, Last-Modified and 304 Not Modified

A stale (or `no-cache`) copy need not be downloaded again. The cache can send a **conditional
request**, if the original response carried a **validator**:

- `ETag`: an identifier for this exact version, such as `"p42-v7"`. Any content change must change it.
- `Last-Modified`: when the resource last changed. It only has one-second resolution.

```http
GET /api/products/42 HTTP/1.1
Host: api.example.com
If-None-Match: "p42-v7"

HTTP/1.1 304 Not Modified
ETag: "p42-v7"
Cache-Control: max-age=10, s-maxage=60
```

A `304 Not Modified` has no body: the cache refreshes its stored headers and reuses its copy. (With
`Last-Modified`, the request header is `If-Modified-Since`.) Web servers usually generate validators
for static files. In your app, hashing the response body saves bandwidth but not CPU, because you must
build the body first. A database version number lets you skip the rendering too:

```python
def get_product(request, product_id):
    product = db.load_product(product_id)        # version column is bumped on every update
    etag = f'"p{product.id}-v{product.version}"'
    headers = {"ETag": etag, "Cache-Control": "max-age=10, s-maxage=60"}

    # Simplified: real code should also accept a list of ETags, "*" and the W/ prefix
    if request.headers.get("If-None-Match") == etag:
        return Response(status=304, headers=headers)  # no body, no JSON rendering
    return Response(render_json(product), status=200, headers=headers)
```

The version number only tracks the data. If a deploy changes the JSON format, put a format version
in the ETag as well (such as `"p42-v7-f2"`), or clients keep the old shape.

> [!TIP]
> Behind a load balancer, all servers must produce the **same** ETag for the same content. ETags built
> from machine-local details (such as file inode numbers) differ between servers, so a revalidation
> that reaches another server gets a full `200` response instead of a cheap `304`.

## Vary and the cache key

A cache matches requests to stored responses with a **cache key**: by default, the method and the
full URL. If the response also depends on a request header, say so with `Vary`. For example,
`Vary: Accept-Encoding` (normal and cheap) means "keep one copy per value of `Accept-Encoding`", so a
client that cannot decompress Brotli never receives a Brotli body.

- `Vary: Cookie` or `Vary: User-Agent` creates roughly one copy per user or per browser version, and
  your **hit ratio** (the share of requests served from cache) collapses. Prefer putting variants in
  the URL, such as `/fr/pricing` instead of varying on `Accept-Language`.
- `Vary: *` means the stored response can never be reused.

CDNs usually let you shape the key directly: ignore tracking parameters like `utm_source`, sort query
parameters, or add one cookie value (such as a currency).

> [!WARNING]
> Anything that changes the response but is **not** in the cache key is a bug, and can be a security
> hole. If your app builds links from an `X-Forwarded-Host` header that the cache ignores, one attacker
> request can store a poisoned page for every visitor: **web cache poisoning**.

## Static assets: fingerprint them and cache for a year

The most effective caching pattern on the web:

1. The build tool puts a hash of each file's content into its name: `app.3f9a1c.js`. This is
   **fingerprinting**. New content means a new name.
2. Serve these files with `public, max-age=31536000, immutable` (one year, a common convention). They
   never need revalidation, because a new version gets a new URL.
3. Serve the HTML that references them with `no-cache` and an ETag. Browsers check it on every visit
   (usually a cheap 304) and see new file names right after a deploy.

Keep old assets for a while after each deploy. A user who loaded the old HTML a minute earlier will
still request `app.3f9a1c.js`. Upload new files first, switch the HTML second, and delete old files
days later (see [deployment strategies](/posts/deployment-strategies-and-zero-downtime-migrations)).

## CDNs: caching at the edge

**Defaults differ.** Some CDNs cache only well-known static file types unless you opt in. Check what
yours caches by default, its default cache key (some leave the query string out), and how it treats
`Set-Cookie`.

**Separate lifetimes.** You often want a long lifetime at the CDN, which you can purge, and a short
one in browsers, which you cannot: `max-age=60, s-maxage=86400`. Remember the `Age` rule: once the
CDN copy is older than 60 seconds, browsers see it as already stale and revalidate it on each use.
For rules that only CDNs read, there is `CDN-Cache-Control` (RFC 9213), supported by several CDNs,
and the older `Surrogate-Control`, which Fastly reads.

### Purging (invalidation)

A **purge** tells the CDN to drop stored copies before their lifetime ends:

- **By URL** or URL prefix.
- **By tag.** The origin labels responses in a header (`Surrogate-Key` at Fastly, `Cache-Tag` at
  Cloudflare) with tags such as `product-42` and `category-7`. After product 42 changes, one purge
  call removes every page that showed it.
- **Everything.** A last resort: all traffic suddenly goes to your origin.

A purge reaches the CDN, not browsers. Use purges for HTML and API responses, and new URLs for
assets. Some CDNs offer a "soft purge" that marks content stale instead of deleting it.

### Origin shield and tiered caching

Each PoP has its own cache, so with 100 PoPs one new or purged URL can cause 100 origin misses.
A **shield** (or **tiered cache**) adds a layer: edge PoPs ask one chosen PoP, and only that PoP
talks to the origin.

```text
 edge Tokyo  ---+
 edge Paris  ---+--->  shield PoP (near origin)  --->  origin
 edge Lima   ---+      one miss per URL, not one per edge
```

CloudFront calls this Origin Shield, Fastly calls it shielding, and Cloudflare calls it Tiered Cache.
Many CDNs (and nginx with `proxy_cache_lock on`) also **collapse requests**: when many requests for
the same uncached URL arrive together, one goes to the origin and the rest wait for its answer.

### Caching API responses

APIs can use a CDN too, when the response is identical for everyone who requests the same URL:
catalogues, exchange rates, live scores. Even a tiny lifetime helps. With
`s-maxage=5`, each PoP asks your origin about once per URL every 5 seconds, whether 10 or 10,000
users are asking. This is often called **micro-caching**. Cache only `GET` and `HEAD`, and make the
URL fully describe the response: filters, page and sort order go in the query string, and the query
string must be part of the CDN's cache key (see
[API pagination and versioning](/posts/api-design-pagination-versioning)). Do not micro-cache data
that must be exact at the moment it is read, such as a balance shown right before a payment.

## Never cache personalised responses publicly

A shared cache only sees a URL. If Alice's `/account` page was stored publicly once, the next visitor
to `/account` may get it.

- Anything that depends on who the user is gets `private` (only the browser may store it) or
  `no-store`.
- A shared cache will not reuse a response to a request with an `Authorization` header **unless** the
  response says `public`, `s-maxage` or `must-revalidate`. Do not add those to authenticated
  responses "for performance".
- Session cookies are not in the default cache key. A page that reads the cookie to show "Hi, Alice"
  looks like any other URL to the CDN.

**`Set-Cookie` is a classic trap.** RFC 9111 does not forbid caching a response that contains
`Set-Cookie`. If a shared cache stores one, it can hand the same cookie, perhaps a session id, to
every later visitor. Many CDNs and proxies (nginx, for example) refuse to cache such responses by
default, but that is a product choice, not a rule. Watch for frameworks that start an anonymous
session on every page. Never set cookies on responses that a shared cache may store.

A good pattern: cache the public page for everyone, and load the personal parts (name, cart count)
from a small `/api/me` request marked `private, no-cache`. For sessions in general, see
[sessions vs JWT](/posts/authentication-sessions-vs-jwt).

> [!WARNING]
> This is not a theoretical risk. Valve has described a December 2015 incident in which a caching
> configuration change, made during a denial-of-service attack, caused Steam store pages with
> personal account details to be shown to other users.

## Debugging: read the response headers

`curl` shows the headers without your browser's cache in the way. Run it twice and compare:

```sh
curl -s -o /dev/null -D - https://www.example.com/assets/app.3f9a1c.js
```

`-D -` prints the headers and `-o /dev/null` drops the body. Then read these headers:

| Header | What it tells you |
|---|---|
| `Age` | Seconds since the origin generated or last validated the response. Missing or `0` usually means it came from the origin. |
| `Cache-Status` | The standard header (RFC 9211), for example `ExampleCDN; hit` or `ExampleCDN; fwd=uri-miss`. Not every CDN sends it yet. |
| `CF-Cache-Status` | Cloudflare: `HIT`, `MISS`, `EXPIRED`, `BYPASS`, `DYNAMIC` and others. |
| `X-Cache` | CloudFront (`Hit from cloudfront`), Fastly and others; values vary by vendor. |

Always a miss? Look for `Set-Cookie`, `private`, `no-store`, a wide `Vary` or unique query strings.
`curl` only reaches the PoP nearest to you. In the browser's developer tools, keep **Disable cache**
off and, in the Network tab, look for 304 statuses and responses served from the browser's cache
(Chrome shows "(disk cache)" or "(memory cache)").

## Cheat sheet

| Response | `Cache-Control` | Notes |
|---|---|---|
| Fingerprinted assets (`app.3f9a1c.js`) | `public, max-age=31536000, immutable` | Keep old files after deploys |
| HTML, single-page app `index.html` | `no-cache` | Send an `ETag`; checks are cheap 304s |
| Public page, may be slightly stale | `max-age=60, s-maxage=600, stale-while-revalidate=60, stale-if-error=86400` | Purge the CDN when it changes |
| Public API data, same for everyone | `max-age=0, s-maxage=10, stale-while-revalidate=30` | Nothing caller-specific inside |
| Logged-in pages, `/api/me` | `private, no-cache` | Never `public` or `s-maxage` |
| Sensitive data (tokens, banking) | `no-store` | |
| Errors (5xx) | `no-store` | Don't let an outage stay cached |

## Common mistakes

- **A long `max-age` on a URL whose content can change**, like `/app.js`. Browsers cannot be purged.
- **`no-store` everywhere**, which throws away cheap 304 revalidations.
- **`public` or `s-maxage` on personal responses**, or `Set-Cookie` on cacheable ones.
- **Middleware that adds a long `max-age` to every response**, errors included.
- **Accidental permanent redirects.** `301` and `308` may be cached even without a `Cache-Control`
  header, and browsers may keep them for a long time. Use `302` or `307` until you are sure.

## Further reading

- RFC 9111: [HTTP Caching](https://www.rfc-editor.org/rfc/rfc9111), plus RFC 5861
  ([stale-while-revalidate and stale-if-error](https://www.rfc-editor.org/rfc/rfc5861)) and RFC 8246
  ([immutable](https://www.rfc-editor.org/rfc/rfc8246))
- MDN: [HTTP caching](https://developer.mozilla.org/en-US/docs/Web/HTTP/Caching)
- Jake Archibald: [Caching best practices & max-age gotchas](https://jakearchibald.com/2016/caching-best-practices/)
- Mark Nottingham: [Caching Tutorial for Web Authors and Webmasters](https://www.mnot.net/cache_docs/)
- RFC 9211: [Cache-Status](https://www.rfc-editor.org/rfc/rfc9211) and RFC 9213:
  [Targeted HTTP Cache Control](https://www.rfc-editor.org/rfc/rfc9213)
- PortSwigger: [Web cache poisoning](https://portswigger.net/web-security/web-cache-poisoning)
