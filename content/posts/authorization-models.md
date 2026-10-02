+++
title = "Authorization: RBAC, ABAC and relationship-based access control"
summary = "How apps decide who may do what: ACLs, roles, attributes and Zanzibar-style relationships, policy engines like OPA and Cedar, tenant isolation with PostgreSQL row-level security, and how to stop the most common API bug (IDOR)."
tags = ["security","architecture"]
level = "intermediate"
date = 2026-10-02
+++

A customer logs in to your invoicing app and opens `/invoices/1042`. Out of curiosity, they change the
URL to `/invoices/1043`, and they see another company's invoice: names, addresses, amounts. The login
worked perfectly. What failed was the next question: *is this user allowed to see this invoice?* That
question is **authorization**. Getting it wrong is one of the most common serious bugs in web APIs.

This article covers the main ways to model permissions, where to enforce them, how to keep customers'
data apart, and how to make the "change the ID" bug hard to write.

## Authentication vs authorization

- **Authentication** (often shortened to *authn*) answers "who are you?". Passwords, session cookies
  and tokens belong here. See [authentication basics](/posts/authentication-sessions-vs-jwt).
- **Authorization** (*authz*) answers "may this user do this action on this resource, right now?".

Every authorization decision has the same shape:

```text
allowed?( subject,    action,      resource,       context )
          user 7      "approve"    invoice 1043    time, IP address, tenant, ...
```

HTTP has a status code for each failure. `401 Unauthorized` really means "not authenticated" (the name
is misleading). `403 Forbidden` means "I understood the request, and the answer is no".

> [!NOTE]
> OAuth 2.0 **scopes** such as `invoices:read` limit what a *client application* may do for a user.
> They do not replace checking what the *user* may do. See
> [OAuth 2.0 and OpenID Connect](/posts/oauth2-and-openid-connect).

## Access control lists (ACLs)

The oldest model: each resource carries a list of who may do what with it.

```text
document 42   ->  alice: owner,  bob: edit,  sales-team: view
document 43   ->  carol: owner
```

Unix file permissions and a "Share with..." dialog are simple ACLs. They are perfect for sharing single
objects. They become painful for organisation-wide rules ("all accountants can read every invoice"
means editing every invoice's list) and for the reverse question, "what can Bob access?".

## RBAC: role-based access control

RBAC adds one level of indirection. Users get **roles**, and roles get **permissions**.

```text
users              roles                permissions
alice ---------->  admin       ------>  invoice:read, invoice:write, user:manage
bob   ----+
carol ----+----->  accountant  ------>  invoice:read, invoice:write, invoice:approve
dave  ---------->  viewer      ------>  invoice:read
```

In a SaaS product the role is almost always **scoped to a tenant** (a customer organisation): Bob is an
accountant *in Acme Ltd*, not everywhere.

```sql
-- Tables: memberships(user_id, org_id, role) and role_permissions(role, permission)
-- May user 7 approve invoices in organisation 3?
SELECT EXISTS (
    SELECT 1 FROM memberships m
    JOIN role_permissions rp ON rp.role = m.role
    WHERE m.user_id = 7 AND m.org_id = 3 AND rp.permission = 'invoice:approve'
);
```

> [!TIP]
> In code, check **permissions**, not roles: `require(user, "invoice:approve")` instead of
> `if user.role == "admin"`. Then a new role is a data change, not a search through the codebase.

RBAC is easy to understand and to audit. Its limit: roles describe *kinds of people*, not
*relationships to specific things*. Real rules soon sound like "accountants can approve invoices
**under 10,000**, **in their own region**, **unless they created the invoice**". Encode each variation
as a role and you get **role explosion**: `accountant-eu`, `accountant-eu-senior`,
`accountant-us-readonly`... hundreds of roles that nobody can reason about.

## ABAC: attribute-based access control

ABAC decides with **policies** (rules) over **attributes**:

- of the **subject**: department, region, employment type;
- of the **resource**: owner, region, amount, status, classification;
- of the **environment**: time of day, IP address, whether the user used MFA.

The invoice rule becomes one policy instead of many roles:

```text
allow if  user has permission "invoice:approve"
     and  invoice.region == user.region
     and  invoice.amount < 10000
     and  invoice.created_by != user.id
```

Most apps already do a little ABAC: "users can edit their own comments" is an attribute rule. NIST
Special Publication 800-162 (*Guide to Attribute Based Access Control (ABAC) Definition and
Considerations*) is a widely cited description.

The price: "who can see this record?" no longer has a simple answer. And you need the attributes at
decision time, which often means loading the resource *before* you can decide.

## ReBAC: relationship-based access control

In many products, permissions **follow relationships**. Alice is a member of the Sales team. Sales can
edit the "Q3" folder. The folder contains a document. So Alice can edit the document. File sharing,
project tools and code hosting all work like this.

Google described its system for this in a 2019 paper, **"Zanzibar: Google's Consistent, Global
Authorization System"**. Zanzibar stores permissions as **relation tuples** of the form
`object#relation@user`, and answers a check by walking the graph:

```text
document:q3-plan#parent@folder:q3        the document is inside folder q3
folder:q3#editor@team:sales#member       members of team sales are editors of folder q3
team:sales#member@user:alice             alice is a member of team sales

check(user:alice, editor, document:q3-plan)

user:alice --member--> team:sales --editor--> folder:q3 --parent of--> document:q3-plan  => ALLOW
```

A schema says which relations imply which permissions. Open-source systems inspired by the paper
include **OpenFGA** and **SpiceDB** (by AuthZed). This is a model in OpenFGA's language:

```text
model
  schema 1.1

type user

type team
  relations
    define member: [user]

type folder
  relations
    define editor: [user, team#member]
    define viewer: [user, team#member] or editor

type document
  relations
    define parent: [folder]
    define editor: [user] or editor from parent
    define viewer: [user] or editor or viewer from parent
```

Your application writes tuples when something happens ("Alice shared the folder with Sales") and
asks for a check before acting. For list pages, both systems also answer "which documents can Alice
view?" (`ListObjects` in OpenFGA, `LookupResources` in SpiceDB).

The paper also solves the **"new enemy" problem**: if you remove Bob from a folder and then add a
new file to it, a check that uses old data (for example, from a replica that is behind) must not let
Bob see the file. In Zanzibar, the application stores a consistency token (a "zookie") with each new
version of the content. Later checks send the token back, and Zanzibar evaluates them on data that
is at least that fresh. SpiceDB's equivalent is the ZedToken.

The costs: a new service on the path of almost every request, and relationships that live in two
places (your database and the authorization store) and must stay in sync. That is the dual-write
problem covered in
[sagas and the outbox pattern](/posts/distributed-transactions-saga-outbox) and
[change data capture](/posts/change-data-capture).

## Comparing the models

| Model | Decides using | Good for | Struggles with |
|---|---|---|---|
| ACL | A list on each object | Sharing single items | Organisation-wide rules; "what can Bob access?" |
| RBAC | User → role → permission | Admin consoles, internal tools, B2B apps with fixed roles | Per-object and conditional rules (role explosion) |
| ABAC | Rules over attributes | Conditions on region, amount, time, status | Answering "who can access X?"; debugging |
| ReBAC | A graph of relationships | Sharing, folders, nested teams | Extra service, keeping data in sync |

The models mix freely; most real systems combine two or three.

## Policy engines: decisions as code

When rules grow, teams move them out of scattered `if` statements into a **policy engine**. Your
code becomes the **Policy Enforcement Point** (it allows or blocks the request), and the engine is the
**Policy Decision Point** (it evaluates the rules).

```text
 request   +-----------------+  "may user 7 approve invoice 1043?"   +------------------+
---------> |  API service    | ------------------------------------> |  policy engine   |
           |  (enforcement)  |    + attributes of user and invoice   |  (decision)      |
           |                 | <------------------------------------ |  rules as code   |
           +--------+--------+             allow / deny              +------------------+
                    |
                    v
                database
```

**Open Policy Agent (OPA)** is a general-purpose engine with its own language, **Rego**. It is also
widely used in Kubernetes, for example to reject manifests that break your rules (the Gatekeeper
project).

```rego
# Rego syntax for OPA 1.0 and later
package invoices

default allow := false

allow if {
    "invoice:approve" in input.user.permissions
    input.invoice.region == input.user.region
    input.invoice.amount < 10000
    input.invoice.created_by != input.user.id
}
```

**Cedar**, open-sourced by AWS and used by Amazon Verified Permissions, has a deliberately smaller
language that is designed to be fast and easy to analyse. Anything not permitted is denied, and a
`forbid` rule always wins over a `permit` rule:

```text
permit (principal, action == Action::"approve", resource)
when { resource.region == principal.region && resource.amount < 10000 };

forbid (principal, action == Action::"approve", resource)
when { resource.created_by == principal };
```

An engine gives you one place for rules, policy tests and decision logs. It usually does **not** read
your database: you send it the attributes (the user's region, the invoice's amount) with each
question, or copy data into it ahead of time. For one application with a few rules, a
well-organised module in your own code is often enough.

## Multi-tenant isolation

In B2B SaaS, the worst authorization bug is a **cross-tenant leak**: one customer sees another
customer's data. There are three common layouts:

| Layout | Isolation | Cost and effort |
|---|---|---|
| Database per tenant | Strongest; easy per-customer backup and deletion | Many databases to migrate and monitor |
| Schema per tenant | Good | Every migration runs once per tenant |
| Shared tables with a `tenant_id` column | Only as good as every query | Cheapest; very common |

For shared tables:

- Take the tenant from the **authenticated session**, never from a URL, header or body field the
  client controls.
- Put `tenant_id` in every tenant-owned table and filter by it in every query. It is usually the
  first column of multi-column indexes, so per-tenant queries stay fast (see
  [indexes](/posts/database-indexes-and-explain)).
- Remember the places that are not SQL: cache keys, search indexes, file paths, background jobs and
  exports.

### A safety net in the database: PostgreSQL row-level security

Row-level security (RLS) lets PostgreSQL add a filter to every query on a table. A forgotten
`WHERE tenant_id = ...` then returns nothing, instead of every customer's data.

```sql
ALTER TABLE invoices ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation ON invoices
    USING      (tenant_id = NULLIF(current_setting('app.tenant_id', true), '')::bigint)
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant_id', true), '')::bigint);

-- Per request, inside a transaction:
BEGIN;
SELECT set_config('app.tenant_id', '42', true);   -- true = only for this transaction
SELECT * FROM invoices WHERE id = 1043;           -- RLS adds: AND tenant_id = 42
COMMIT;
```

Details that bite:

- **Superusers, `BYPASSRLS` roles and the table owner skip RLS.** Connect as a role that does not
  own the tables. `ALTER TABLE ... FORCE ROW LEVEL SECURITY` makes the policies apply to the owner
  too, but superusers and `BYPASSRLS` roles still skip them.
- **Views can skip RLS.** By default, a view reads its tables with the permissions of the view's
  owner. If that owner skips RLS (for example, it owns the tables), queries through the view skip it
  too. In PostgreSQL 15 and later, `WITH (security_invoker = true)` makes a view use the
  permissions of the role that runs the query instead.
- **Connection pools reuse connections**, so a plain `SET` would leak the tenant to the next request.
  Use transaction-local settings (`set_config(..., true)` or `SET LOCAL`). See
  [connection pooling](/posts/connection-pooling).
- **If the setting is missing, the comparison is with NULL and no rows match.** The policy fails
  closed, which is what you want.
- `USING` filters rows you read, update or delete; `WITH CHECK` validates rows you write.
- RLS knows tenants, not business rules. It is a backstop under your application checks.

## The bug you will actually ship: broken object level authorization

Back to the opening story. Changing an ID in a request and receiving someone else's object is called
**IDOR** (insecure direct object reference). OWASP's API Security Top 10 calls it **broken object
level authorization (BOLA)** and lists it first, as API1, in its 2023 edition.

It happens because the route checks *that* you are logged in, but not *whether this object is yours*:

```python
# Vulnerable: any logged-in user can read any invoice
@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: int, user=Depends(current_user)):
    return db.one("SELECT * FROM invoices WHERE id = %s", invoice_id)

# Fixed: scope the lookup to the caller's tenant, then apply finer rules
@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: int, user=Depends(current_user)):
    invoice = db.one("SELECT * FROM invoices WHERE id = %s AND tenant_id = %s",
                     invoice_id, user.tenant_id)
    if invoice is None:
        raise HTTPException(404)   # same answer for "missing" and "not yours"
    authorize(user, "invoice:read", invoice)
    return invoice
```

Returning `404` instead of `403` avoids confirming that the object exists. GitHub's REST API
documentation says it does this in some places, so that it does not reveal private repositories.

What does **not** fix it:

- **Random IDs (UUIDs).** They make guessing harder, but IDs leak through URLs, logs, emails and
  shared links. They are extra protection, not authorization.
- **Hiding the button in the UI.** Attackers call the API directly.

Related problems (the first two are also in the OWASP list):

- **Broken object property level authorization (API3):** a user may edit their profile, and sending
  `"role": "admin"` in the body works too (*mass assignment*). Or the response includes fields the
  user should not see. Accept and return only explicit lists of fields.
- **Broken function level authorization (API5):** admin endpoints that only check "logged in".
- **Nested routes:** for `/orgs/1/projects/9`, check that project 9 belongs to organisation 1, not
  only that the user is a member of organisation 1.

## Centralise the checks, then test them

Checks spread over 200 handlers will miss one. Layer them:

```text
 gateway / middleware   valid session?  may this client call /admin/*?          (coarse)
          |
 service layer          authorize(user, action, resource) on every read/write   (fine-grained)
          |
 database               tenant filter + row-level security                      (backstop)
```

- **One function, one vocabulary.** Handlers, background jobs, GraphQL resolvers, webhooks and
  exports all call the same `authorize(subject, action, resource)`, or the same policy engine.
- **Deny by default.** No matching rule means no access.
- **Log decisions** for sensitive actions: who, what, which object, the result and why (see
  [observability](/posts/observability-logs-metrics-traces)).
- **Watch for stale permissions.** A JWT carrying `"role": "admin"` keeps working after you demote
  the user, until it expires. So do cached decisions (see [caching](/posts/caching-strategies)).

Then test authorization like any other feature:

- **A permission matrix**: role × action → expected result, as a parametrised test.
- **The two-tenant test**: as a user of tenant B, call every endpoint that takes an ID with tenant
  A's IDs and expect `404`. Generate the cases from your route list so new endpoints are covered
  automatically.
- **Policy tests**: OPA has a built-in `opa test` command; rules in your own code get ordinary unit
  tests.

## In practice: choosing a model

1. Start with **tenant-scoped RBAC** plus ownership checks, behind one `authorize` function.
2. Add **attribute conditions** there when rules need them (amount, status, region).
3. Move to **ReBAC** when users share objects and permissions are inherited through folders, projects
   or nested teams.
4. Adopt a **policy engine** when many services must apply the same rules, or a security team must
   review rules separately from application code.
5. With shared tables in PostgreSQL, add **row-level security** as a backstop.

## Common mistakes

- Trusting the client for `tenant_id`, `user_id` or `role`.
- Filtering list endpoints by tenant, but not detail, update, delete or export endpoints.
- Forgetting paths that are not HTTP handlers: queue consumers, cron jobs, file downloads.
- Treating OAuth scopes as user permissions.

## Further reading

- Pang et al.: [Zanzibar: Google's Consistent, Global Authorization System](https://www.usenix.org/conference/atc19/presentation/pang) (USENIX ATC 2019)
- OWASP API Security Top 10 2023: [API1 Broken Object Level Authorization](https://owasp.org/API-Security/editions/2023/en/0xa1-broken-object-level-authorization/)
- OWASP: [Authorization Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html)
- PostgreSQL documentation: [Row Security Policies](https://www.postgresql.org/docs/current/ddl-rowsecurity.html)
- Policy engines: [Open Policy Agent](https://www.openpolicyagent.org/) and [Cedar](https://www.cedarpolicy.com/)
- Zanzibar-style systems: [OpenFGA](https://openfga.dev/) and [SpiceDB](https://github.com/authzed/spicedb)
