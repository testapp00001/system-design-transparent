+++
title = "Authentication basics: password hashing, sessions vs JWT, cookies and CSRF"
summary = "How to store passwords (Argon2id/bcrypt), how server-side sessions and JWTs really differ, which cookie flags matter, and how to stop cross-site request forgery — explained with the choices this site made."
tags = ["security", "backend", "api-design"]
level = "intermediate"
date = 2026-10-02
+++

Authentication is the part of an app where a small mistake becomes a headline. The good news: the
right answers are well known and mostly boring. This article covers storing passwords, keeping users
logged in (sessions vs tokens), cookie settings and CSRF — and points at this site's own code as a
small working example.

## Storing passwords

**Never store passwords**, not even encrypted. Store a **slow, salted hash** so that a stolen database
doesn't reveal passwords.

- **Salted**: each password gets a random salt, so identical passwords produce different hashes and
  precomputed tables (rainbow tables) are useless.
- **Slow on purpose**: a fast hash like SHA-256 can be computed billions of times per second on a GPU,
  making brute force cheap. Password hashing functions are deliberately expensive and, ideally,
  **memory-hard** (expensive on GPUs too).

Use, in order of preference (OWASP's current guidance): **Argon2id**, **scrypt**, **bcrypt**, or
PBKDF2 where FIPS compliance requires it. Use your language's well-maintained library with its default
or recommended parameters; the output string encodes algorithm, parameters and salt, so you can
increase cost later and rehash on next login.

```text
$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>      <- what goes in the database
```

A few details that are easy to miss:

- Hashing is CPU-heavy: in async servers, run it on a blocking thread pool so it doesn't stall other
  requests (this site does that in `src/auth.rs`).
- **Limit password length** (e.g. 128 characters) so nobody can submit megabytes of input to make you
  hash it, and **rate-limit login** attempts per account and per IP.
- On login with an unknown username, still run a hash verification against a dummy hash so that
  response time doesn't reveal which usernames exist.

## Staying logged in: sessions

After a successful login, the classic approach:

1. Generate a long random **session token** (e.g. 32 bytes from a cryptographically secure RNG).
2. Store a session record server-side: token (or better, a **hash of the token**), user id, expiry.
3. Send the token in a cookie.
4. On each request, look up the session; if valid, you know the user.

```text
browser                     server                         database
  | POST /login ---------->  | verify password               |
  |                          | token = random(32 bytes)      |
  |                          | INSERT session(sha256(token), user, expires) -->
  | <-- Set-Cookie: session=token; HttpOnly; Secure; SameSite=Lax
  | GET /me  Cookie: session=token --> lookup sha256(token) -------------------> user 42
```

- **Revocation is easy**: delete the row and the user is logged out everywhere, immediately ("log out
  all devices", password change, account ban).
- **Costs a lookup per request** — an indexed primary-key read, which is cheap; cache it if needed.
- Storing only the token's hash means a leaked database can't be used to hijack sessions.

## Staying logged in: JWTs

A **JSON Web Token** is a signed (sometimes encrypted) piece of JSON — claims like user id and expiry —
that the server can verify **without a database lookup**:

```text
eyJhbGciOiJFUzI1NiJ9 . eyJzdWIiOiI0MiIsImV4cCI6MTc5MDAwMDAwMH0 . <signature>
     header                    payload (NOT encrypted, just base64)
```

- **Stateless verification**: any service with the public key can check it. Useful across many
  services or when an identity provider issues tokens for APIs it doesn't run.
- **Revocation is hard**: a JWT is valid until it expires. To log someone out early you need a denylist
  (a lookup per request — the thing you were avoiding) or very short lifetimes plus refresh tokens
  (which are stored server-side and revocable).
- **Payloads are readable** by anyone holding the token: never put secrets in them.
- Implementation pitfalls are common: accepting `alg: none`, confusing symmetric and asymmetric keys,
  not validating expiry, audience or issuer. Use a mature library and pin the expected algorithm.

### Which one?

| | Server-side session | JWT (access token) |
|---|---|---|
| Logout / revoke | Immediate | Wait for expiry, or add a denylist |
| Per-request cost | DB/cache lookup | Signature check |
| Size | Small random id | Hundreds of bytes+ |
| Best for | Web apps with their own backend | APIs across many services, third-party/mobile clients, SSO via OAuth/OIDC |

For a typical web application with its own backend: **server-side sessions in an HttpOnly cookie**.
"JWT in localStorage" is a common pattern in tutorials, and it's worse: any XSS can read the token and
send it anywhere. Use JWTs where their statelessness solves a real problem — usually as short-lived
access tokens issued by an OAuth 2.0 / OpenID Connect provider.

## Cookie flags that matter

```http
Set-Cookie: session=...; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=2592000
```

- **HttpOnly** — JavaScript can't read it, so XSS can't steal it (it can still *use* it while the page
  is open, so preventing XSS still matters).
- **Secure** — only sent over HTTPS.
- **SameSite=Lax** — not sent on cross-site subrequests and cross-site POSTs, which blocks most CSRF.
  `Strict` is stronger but drops the cookie when users follow links from other sites.
- **Max-Age / Expires** — set a sensible lifetime; also expire sessions server-side.
- Consider the `__Host-` cookie name prefix, which forces `Secure`, `Path=/` and no `Domain`.

## CSRF: cross-site request forgery

If a user is logged in to `bank.example`, a malicious page could submit a hidden form to
`bank.example/transfer`. The browser attaches the user's cookies, and the bank sees a legitimate-looking
request.

Defences, layered:

1. **SameSite cookies** (Lax by default in modern browsers) stop cookies being sent on cross-site POSTs.
2. **Check where the request came from**: modern browsers send **Fetch Metadata** headers
   (`Sec-Fetch-Site: same-origin | same-site | cross-site | none`) and an `Origin` header on POSTs.
   Reject unsafe methods whose `Sec-Fetch-Site` is `cross-site` (or whose `Origin` doesn't match your
   host). This site does exactly this in `src/security.rs` — which also protects its anonymous voting
   endpoints, where there is no cookie at all.
3. **CSRF tokens**: a random value embedded in forms and verified on submit — the traditional
   defence, still built into many frameworks.
4. Never change state on `GET` requests.

## Other essentials

- **Rate-limit** login, registration and password reset (see [rate limiting](/posts/rate-limiting)).
- **Rotate the session id at login** (prevents session fixation) and on privilege changes.
- **Generic error messages**: "invalid username or password", not "user not found".
- **Offer MFA** (TOTP, passkeys/WebAuthn) for anything valuable; passkeys remove passwords entirely.
- **Redirect safely** after login: only allow relative paths in `?next=` to avoid open redirects
  (`//evil.example` is *not* a relative path).
- **Security headers**: a Content Security Policy to limit XSS damage, `X-Content-Type-Options: nosniff`,
  `frame-ancestors` to prevent clickjacking.
- Consider **not building auth yourself**: an identity provider (OIDC) or your framework's built-in
  auth is often the safest choice.

## Further reading

- OWASP Cheat Sheets: [Password Storage](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html), [Session Management](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html), [Cross-Site Request Forgery Prevention](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html)
- web.dev: [Protect your resources from web attacks with Fetch Metadata](https://web.dev/articles/fetch-metadata)
- MDN: [Set-Cookie](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Set-Cookie)
- RFC 8725: [JSON Web Token Best Current Practices](https://www.rfc-editor.org/rfc/rfc8725)
