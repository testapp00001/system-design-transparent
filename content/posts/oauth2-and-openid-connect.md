+++
title = "OAuth 2.0 and OpenID Connect explained for backend developers"
summary = "What really happens behind \"Sign in with Google\": the four OAuth roles, the authorization code flow with PKCE, client credentials, access vs refresh vs ID tokens, how to validate them, where SPAs should keep them, and the mistakes that cause breaches."
tags = ["security","api-design"]
level = "intermediate"
date = 2026-10-02
+++

You add a "Sign in with Google" button to your app. The quick-start guide asks for a `client_id`, a
`client_secret` and a `redirect_uri`, and after a few redirects you receive three different tokens.
It works, but nobody on the team can explain what each piece protects against. That is dangerous:
many serious OAuth bugs come from a check that looked optional and was skipped. This article explains
the roles, the flows, the tokens, what OpenID Connect adds, and how to validate everything.

## The problem: access without sharing passwords

Before OAuth, a photo-printing website that wanted your pictures from another service asked for your
password there. It could then do *everything* you can do, and you could not take access back without
changing your password.

**OAuth 2.0** (RFC 6749) solves this with *delegated authorization*. You log in at the provider
itself, approve a limited request ("read your photos"), and the app receives a **token** instead of
your password. The token is limited, it expires, and you can revoke it.

OAuth answers "what may this app do?". It does **not** say who the user is. **OpenID Connect (OIDC)**
is a thin layer on top of OAuth that answers "who just logged in?". "Sign in with Google" is OIDC.

## The four roles

| Role | Meaning | "Sign in with Google" | Your mobile app + your API |
|---|---|---|---|
| **Resource owner** | The person who owns the data and can grant access | The user | The user |
| **Client** | The application that wants access | Your web app | Your mobile app |
| **Authorization server** | Logs the user in, asks for consent, issues tokens | Google's account service | Keycloak, Auth0, Okta, Entra ID… |
| **Resource server** | The API that accepts tokens | Google's APIs | Your API |

A **confidential client** runs on a server and can keep a secret. A **public client** cannot: a
single-page app (SPA) or a mobile app ships its code to users, so anything inside it can be
extracted. Every client is **registered** in advance, which gives it a `client_id` and records its
exact **redirect URIs**: where users may be sent back after login.

## The authorization code flow with PKCE, step by step

Use this flow whenever a user is involved: web apps, SPAs and mobile apps. **PKCE** (Proof Key for
Code Exchange, RFC 7636) is an extension that keeps it safe even for public clients.

```text
 Browser                    Your backend (client)             Authorization server
    |                                 |                                     |
 1  |-- click "Sign in with Google" ->|                                     |
    |                                 | create state, nonce, verifier       |
 2  |<-- 302 to /authorize -----------|                                     |
 3  |-- GET /authorize?client_id&scope&state&nonce&code_challenge --------->|
 4  |                                 |       user logs in, approves scopes |
 5  |<-- 302 to redirect_uri?code=...&state=... ----------------------------|
 6  |-- GET /callback?code&state ---->|                                     |
    |                                 | check state                         |
 7  |                                 |-- POST /token: code + verifier ---->|
    |                                 |     SHA-256(verifier) == challenge? |
 8  |                                 |<-- access + refresh + ID token -----|
    |                                 | validate ID token, find user        |
 9  |<-- Set-Cookie: session=... -----|                                     |
```

1. The user clicks. Your backend creates three random values and keeps them in a short-lived session:
   - `state`: proves that the answer belongs to a login that *this browser* started.
   - `nonce` (OIDC only): is copied into the ID token, so you can match the token to this login.
   - `code_verifier`: the PKCE secret, a random string of 43 to 128 characters.
2. It redirects the browser to the authorization server with the `code_challenge`: the SHA-256 hash
   of the verifier, not the verifier itself.
3. The browser follows the redirect (line breaks added for reading):

   ```http
   GET /authorize?response_type=code&client_id=my-app
       &redirect_uri=https%3A%2F%2Fapp.example.com%2Fcallback
       &scope=openid%20email%20profile&state=af0ifjsldkj&nonce=n-0S6_WzA2Mj
       &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256
   Host: accounts.example.com
   ```

4. The user logs in (password, passkey, MFA; your app sees none of it) and approves the scopes.
5. The browser is sent back to your `redirect_uri` with a one-time **authorization code** and the
   same `state`.
6. Your backend checks that `state` matches the session.
7. It sends the code, the `code_verifier` and its own client credentials to the **token endpoint**,
   in a direct server-to-server call.
8. The authorization server hashes the verifier and compares it with the challenge from step 2. If
   they match, it returns an access token, usually a refresh token and, for OIDC, an ID token.
9. Your backend validates the ID token (see below), finds or creates the user, and starts its own
   session, usually an HttpOnly cookie (see [sessions vs JWT](/posts/authentication-sessions-vs-jwt)).

**Why a code first, and tokens later?** The trip through the browser is the *front channel*: URLs end
up in browser history, logs and `Referer` headers. So only a short-lived, single-use code travels
there (RFC 6749 recommends a lifetime of at most 10 minutes). Tokens travel on the *back channel*: a
direct HTTPS call in which the client also proves who it is.

**Why PKCE?** An attacker may steal the code, for example with a malicious mobile app that registered
the same custom URL scheme. Without the verifier, the code is useless, and only the party that started
the flow has it. RFC 9700 requires PKCE for public clients and recommends it for confidential clients
too. Your OAuth library handles all of this.

## Client credentials: service to service

Sometimes there is no user. A billing job calls the invoices API, acting **for itself**. The
**client credentials** grant is enough:

```http
POST /token HTTP/1.1
Host: auth.example.com
Authorization: Basic <base64(client_id:client_secret)>
Content-Type: application/x-www-form-urlencoded

grant_type=client_credentials&scope=invoices:read
```

You get only an access token: no ID token, and normally no refresh token. In practice:

- **Cache the token** until shortly before it expires. A new token per call adds latency and load.
- **Protect the secret** (see [secrets management](/posts/secrets-management)). Stronger options
  replace it with a JWT signed by the client's private key (`private_key_jwt`) or mutual TLS.
- **One client per service**, with only the scopes it needs.

## Grants you should not use

RFC 6749 also defined two shortcut grants. The **OAuth 2.0 Security Best Current Practice
(RFC 9700)** now advises against both.

| Grant | How it worked | Why it is discouraged |
|---|---|---|
| **Implicit** (`response_type=token`) | The access token came back directly in the redirect URL | Tokens in URLs leak and can be replayed, and cannot be bound to the client. RFC 9700: SHOULD NOT be used. Use code + PKCE. |
| **Resource owner password credentials** | The app collected the user's password and sent it to the token endpoint | It brings back the password problem, and does not work with MFA or passkeys. RFC 9700: MUST NOT be used. |

Devices without a good browser or keyboard (smart TVs, command-line tools) use the **device
authorization grant** (RFC 8628) instead.

## Three kinds of tokens

| | Access token | Refresh token | ID token |
|---|---|---|---|
| Purpose | Call an API | Get new access tokens | Tell the client who logged in |
| Meant for | The resource server (API) | The authorization server only | The client (your app) |
| Format | Opaque string or JWT | Usually opaque | Always a signed JWT |
| Lifetime | Short (minutes to an hour is common) | Longer, revocable | Short; checked once at login |
| Sent to | The API, in `Authorization: Bearer …` | The token endpoint only | Nobody; not to APIs |

An access token is a **bearer token** (RFC 6750): whoever holds it can use it, like cash. DPoP
(RFC 9449) and mutual TLS can bind a token to a key the client holds, so a stolen token alone is
useless. For public clients, RFC 9700 requires refresh tokens to be either bound like this or
**rotated**: each use returns a new refresh token and invalidates the old one.

### Scopes

A **scope** is a string naming a permission the client asks for: `openid`, `email`, `orders:read`.
The user approves them, and the authorization server may grant fewer than requested.

Scopes limit what the **client** may do on the user's behalf. They do not decide what the **user** may
do. A token with `orders:write` does not mean this user may edit order 42; your API still runs its own
checks (see [authorization models](/posts/authorization-models)).

## OpenID Connect: adding identity

OIDC reuses the code flow above. You add the scope `openid`, and you get three extra things.

**1. The ID token**, a signed JWT that describes the login. Its decoded payload:

```json
{
  "iss": "https://accounts.example.com",
  "sub": "248289761001",
  "aud": "my-app",
  "exp": 1790003600,
  "nonce": "n-0S6_WzA2Mj",
  "email": "ana@example.com",
  "email_verified": true
}
```

`iss` is the issuer, `aud` is your `client_id`, and `sub` (subject) is the user's id at that issuer.
OIDC requires that `sub` is never reassigned within an issuer, so **(`iss`, `sub`)** is the key for
the user in your database. Not the email: it can change, and not every provider verifies it.

**2. The userinfo endpoint.** Call it with the access token to get profile claims such as name and
picture. The scopes `profile`, `email`, `address` and `phone` control what it returns.

**3. Discovery.** Each provider publishes a JSON document at
`<issuer>/.well-known/openid-configuration` (Google's is
`https://accounts.google.com/.well-known/openid-configuration`). Libraries read it to find the rest:

```json
{
  "issuer": "https://accounts.example.com",
  "authorization_endpoint": "https://accounts.example.com/authorize",
  "token_endpoint": "https://accounts.example.com/token",
  "jwks_uri": "https://accounts.example.com/jwks"
}
```

`jwks_uri` points to the provider's public signing keys, a **JSON Web Key Set (JWKS)**.

## Validating tokens

Clients checking ID tokens and APIs checking JWT access tokens do the same checks:

1. **Signature**, using the JWKS key whose `kid` (key id) matches the token header. Accept only the
   algorithms you expect, such as `RS256`. Never let the token choose, and never accept `none`.
2. **`iss`** equals the expected issuer exactly.
3. **`aud`** contains *you*: your `client_id` for an ID token, your API's identifier for an access
   token.
4. **`exp`** is in the future, with only a small clock skew allowed (seconds, not hours).
5. **ID token:** `nonce` equals the value you stored when the login started. **Access token:** the
   scopes allow this operation.

With the PyJWT library in Python:

```python
import jwt

jwks = jwt.PyJWKClient("https://accounts.example.com/jwks")

def verify_access_token(token: str) -> dict:
    key = jwks.get_signing_key_from_jwt(token)          # chosen by "kid"
    return jwt.decode(
        token, key.key,
        algorithms=["RS256"],                           # pinned, not read from the token
        issuer="https://accounts.example.com",
        audience="https://api.example.com",             # this API only
        leeway=30,                                      # seconds of clock skew
        options={"require": ["exp", "iss", "aud", "sub"]},
    )
```

Providers rotate their signing keys, so cache the JWKS and refetch it (rate-limited) when a token has
an unknown `kid`.

**Opaque access tokens** cannot be checked locally: the API asks the authorization server's
**introspection endpoint** (RFC 7662) and caches the answer briefly.

> [!NOTE]
> A JWT access token stays valid until it expires, even after logout. Keep it short-lived.

## Where to keep tokens: SPAs and the backend-for-frontend

A server-rendered app keeps tokens on the server. A single-page app has three common options:

1. **localStorage.** Any XSS (cross-site scripting) bug can steal the tokens, refresh token included.
2. **JavaScript memory only.** Better, but tokens are lost on reload, and injected script can still
   use them.
3. **Backend-for-frontend (BFF).** A small server component on your own domain is the confidential
   OAuth client. It runs the code flow, keeps the tokens server-side, gives the browser only an
   HttpOnly session cookie, and attaches the access token to API calls.

```text
Browser (SPA)              BFF (your backend)                   API (resource server)
    |                               |                                   |
    |-- GET /bff/orders ----------->|                                   |
    | Cookie: session=abc           |                                   |
    |                               | session abc -> access token       |
    |                               |-- GET /orders ------------------->|
    |                               | Authorization: Bearer eyJ...      |
    |                               |<-- 200 [...] ---------------------|
    |<-- 200 [...] -----------------|                                   |

 no tokens here        tokens stored and refreshed here
```

The IETF draft *OAuth 2.0 for Browser-Based Applications* describes this pattern and recommends it
for business and sensitive applications. The costs: an extra hop, a server to run, and, because you
are back to cookies, CSRF protection (`SameSite` cookies plus an origin check).

Mobile apps should run code + PKCE in the **system browser**, not an embedded web view (RFC 8252).

## When not to use it (or not to build it yourself)

- **One app with its own users** does not need to *become* an OAuth authorization server; sessions
  are simpler. Add OIDC to *accept* logins from elsewhere: social login or company single sign-on.
- **Do not write your own authorization server** unless that is your product. Use an existing one
  (Keycloak, or a hosted provider) and a maintained client library.

## In practice: a checklist

- [ ] Authorization code + PKCE (`S256`) for every flow with a user. No implicit, no password grant.
- [ ] Redirect URIs registered and compared exactly. No wildcards.
- [ ] `state` checked on the callback; `nonce` checked in the ID token.
- [ ] Signature (pinned algorithms), `iss`, `aud` and `exp` validated on every token.
- [ ] Users keyed by (`iss`, `sub`), not by email.
- [ ] Short-lived access tokens; refresh tokens rotated or bound to a key, never in localStorage.
- [ ] Tokens never appear in URLs or logs.

## Common mistakes

**Using an access token as proof of identity.** A mobile app sends your backend a Google access
token, and your backend logs in whoever userinfo returns for it. But that token may have been issued
to a *different* app: any site where the victim used "Sign in with Google" can replay its token to
log in as the victim. Use the ID token, and check that its `aud` is your `client_id`.

**Not validating `aud`.** An API that accepts "any valid token from our identity provider" also
accepts tokens issued for other apps on that provider, including low-privilege internal tools.

**Loose redirect URI matching.** If the server accepts any URL starting with
`https://app.example.com/`, an attacker can point `redirect_uri` at an open redirect or a
user-controlled page on that domain and collect the code. RFC 9700 requires exact string matching
(with a narrow exception for localhost ports in native apps).

**Missing `state` or `nonce`.** Without `state` (or PKCE), an attacker can make your browser finish a
login with the *attacker's* code. You are silently logged in to their account, and might save your
card details there. Without `nonce`, an ID token from another login can be replayed.

## Further reading

- RFC 6749: [The OAuth 2.0 Authorization Framework](https://www.rfc-editor.org/rfc/rfc6749) and RFC 7636: [Proof Key for Code Exchange](https://www.rfc-editor.org/rfc/rfc7636)
- RFC 9700: [Best Current Practice for OAuth 2.0 Security](https://www.rfc-editor.org/rfc/rfc9700)
- OpenID Foundation: [OpenID Connect Core 1.0](https://openid.net/specs/openid-connect-core-1_0.html) and [OpenID Connect Discovery 1.0](https://openid.net/specs/openid-connect-discovery-1_0.html)
- RFC 9068: [JWT Profile for OAuth 2.0 Access Tokens](https://www.rfc-editor.org/rfc/rfc9068)
- IETF draft: [OAuth 2.0 for Browser-Based Applications](https://datatracker.ietf.org/doc/draft-ietf-oauth-browser-based-apps/)
