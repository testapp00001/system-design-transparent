+++
title = "Feature flags: separating deploy from release"
summary = "How feature flags let you ship code switched off and turn it on later: the four kinds of flags, stable percentage rollouts, a small database-backed implementation, flag debt, and what Knight Capital's 2012 incident teaches about reused flags."
tags = ["devops","architecture","reliability"]
level = "intermediate"
date = 2026-10-02
+++

Your team spent three weeks on a new checkout page. It lives on a long branch, and merging it gets
harder every day. On release day it goes live for everyone at once. If it breaks, the only way back
is to roll back the whole deploy, which also removes two unrelated bug fixes. **Feature flags** fix this. You merge and deploy the new code switched *off*, then switch it
on: for your team first, then 1% of users, then everyone. And you can switch it off again in seconds,
without a deploy.

## Deploy and release are two different things

- **Deploy** means putting a new version of the code on your servers. It is a technical event.
- **Release** means letting users see a new behaviour. It is a product decision.

Without flags, both happen at the same moment. With flags, they are separate:

```text
 time ---------------------------------------------------------------------------->

 code:  merge (flag off)    deploy    deploy    deploy    ...     remove flag + old code
 flag:       off  ->  staff only  ->  1%  ->  10%  ->  50%  ->  100%
                                       |
                        error rate goes up? set it to 0% in seconds, no deploy
```

This lets everyone merge small pieces of unfinished work into the main branch every day
(*trunk-based development*). It also gives you gradual rollouts, where a bug hits 1% of users
instead of all of them, and an off switch that is faster than any rollback (see
[CI/CD and hotfixes](/posts/ci-cd-and-hotfixes)).

In code, a flag is just an `if`. The *value* lives outside the code, in a database or a flag service,
so it can change without a deploy:

```python
if flags.is_enabled("new-checkout", user_id):
    return render_new_checkout(cart)
return render_old_checkout(cart)
```

A useful convention: **off means the old behaviour**. Then the safe default, when the flag system is
unreachable, is always "off".

## Four kinds of flags

Pete Hodgson's article on Martin Fowler's site (linked below) groups flags into four kinds. They
differ mainly in **who changes them** and **how long they should live**.

| Kind | Example | Changed by | Typical lifetime |
|---|---|---|---|
| Release toggle | Hide the unfinished checkout | Developers | Days to weeks, then deleted |
| Experiment | Compare two button texts (A/B test) | Product or data team | Until the result is clear |
| Ops toggle / kill switch | Turn off recommendations when the database is overloaded | On-call engineer | Short, or permanent for kill switches |
| Permission toggle | Beta features for chosen customers | Product or sales | Months, often permanent |

- **Release toggles** are temporary by design. Every week they live, they cost more.
- **Experiments** need each user to stay in the same group, and you must record each user's group.
- **Kill switches** turn off a non-essential feature to protect the core product (see
  [resilience patterns](/posts/resilience-patterns)).
- **Permission toggles** that live forever are really *authorization* rules. Consider moving them into
  your permissions model (see [authorization models](/posts/authorization-models)).

Some tools make lifetimes explicit. Unleash, for example, gives each flag a type with an expected
lifetime and marks flags as "potentially stale" when they live longer than that.

## How a flag decides "on" or "off"

To evaluate a flag, the code passes the flag key and an **evaluation context**: facts about the
current request, such as user id, account id, plan, country or app version. The rules are checked in
order, and the first match wins:

```text
 is_enabled("new-checkout", context: user 42, plan=pro, country=DE)
      |
      v
 flag missing or switched off? ----------------- yes --> off (or the default in code)
      | no
      v
 user or account on the allow list? ------------ yes --> on
      | no
      v
 matches a targeting rule? (staff, plan=beta) -- yes --> the rule's value
      | no
      v
 bucket("new-checkout", 42) < rollout %? ------- yes --> on
      | no
      v
      off
```

### Percentage rollouts need stable hashing

"Turn it on for 10% of users" must **not** mean `random() < 0.10` on every request. A user would see
the new checkout, refresh, and see the old one. Each user needs the same answer on every request and
on every server. The standard trick is to hash the user id into a fixed bucket:

```python
import hashlib

def bucket(flag_key: str, unit_id: str) -> int:
    """A stable number from 0 to 9999 for this flag and this user."""
    digest = hashlib.sha256(f"{flag_key}:{unit_id}".encode()).digest()
    return int.from_bytes(digest[:8], "big") % 10_000

def in_rollout(flag_key: str, unit_id: str, percent: float) -> bool:
    return bucket(flag_key, unit_id) < percent * 100   # 12.5% -> buckets 0..1249
```

Four details matter:

1. **Include the flag key in the hash.** Otherwise the *same* 1% of users get every new feature
   first, and two experiments running together are no longer independent.
2. **Use a stable hash function.** Python's built-in `hash()` on strings is randomised per process
   by default, so two servers would disagree. Use SHA-256, MurmurHash or similar.
3. **Ramp up monotonically.** With `bucket < percent`, every user who was in at 10% is still in at
   25%. Nobody flips back and forth.
4. **Hash the right unit.** Use the user id in consumer apps. In B2B products, use the account id, so
   a whole team sees the same screens. For logged-out visitors, use a cookie or device id.

## Server-side vs client-side evaluation

**Server-side evaluation** happens in your backend. The rules stay private, evaluation is an
in-memory lookup, and users cannot tamper with the result. Prefer it.

Browsers and mobile apps often need flag values too, to show or hide UI. Do **not** send them the
rules: rules can expose unreleased feature names, customer ids and staff email addresses to anyone
with developer tools. Instead, evaluate on the server and send only the results, such as
`{"new-checkout": true}`, with the first page or from an endpoint like `/api/me/flags`. Hosted flag
tools usually offer separate client-side SDKs that work this way.

- **A client-side flag is not a security boundary.** Anyone can change the value in the browser. If
  the flag controls access, prices or limits, the server must check again.
- **Avoid flicker.** If the page renders first and fetches flags later, the old UI jumps to the new
  one. Send the values with the first response.
- **Mobile apps live for a long time.** Old versions stay installed for months. Cache the last values
  on the device and have safe offline defaults.

## A minimal database-backed implementation

You do not need a product to start. One table for the flags, one for an audit log:

```sql
CREATE TABLE feature_flags (
    key          TEXT PRIMARY KEY,                -- 'new-checkout'
    kind         TEXT NOT NULL,                   -- release | experiment | ops | permission
    enabled      BOOLEAN NOT NULL DEFAULT false,  -- master switch
    rollout_pct  NUMERIC(5,2) NOT NULL DEFAULT 0 CHECK (rollout_pct BETWEEN 0 AND 100),
    allow_ids    TEXT[] NOT NULL DEFAULT '{}',    -- always on for these users or accounts
    owner        TEXT NOT NULL,                   -- the team that will remove it
    remove_by    DATE                             -- NULL only for permanent flags
);

CREATE TABLE feature_flag_changes (              -- who changed what, when, and why
    id          BIGSERIAL PRIMARY KEY,
    flag_key    TEXT NOT NULL,
    changed_by  TEXT NOT NULL,
    reason      TEXT NOT NULL,
    new_value   JSONB NOT NULL,
    changed_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Do **not** query the database on every check; one request may check ten flags. Each application
instance loads the whole (small) table into memory and refreshes it in the background:

```python
import logging, threading, time

class FlagStore:
    def __init__(self, db, refresh_seconds=10):
        self._db = db
        self._flags = {}                        # key -> row; replaced as a whole
        threading.Thread(target=self._refresh_loop, args=(refresh_seconds,), daemon=True).start()

    def _refresh_loop(self, every):
        while True:
            try:
                rows = self._db.fetch_all(
                    "SELECT key, enabled, rollout_pct, allow_ids FROM feature_flags")
                self._flags = {r.key: r for r in rows}
            except Exception:
                logging.exception("flag refresh failed; keeping last known values")
            time.sleep(every)

    def is_enabled(self, key, unit_id, default=False):
        flag = self._flags.get(key)
        if flag is None:
            return default                      # unknown flag, or not loaded yet
        if not flag.enabled:
            return False
        if unit_id in flag.allow_ids:
            return True
        return in_rollout(key, unit_id, float(flag.rollout_pct))
```

Design notes:

- **Changes arrive within one refresh interval.** Ten seconds suits most flags. For faster kill
  switches, poll more often (the table is tiny) or use PostgreSQL `LISTEN`/`NOTIFY` to trigger an
  immediate reload.
- **Keep the last known values when the database is down.** A flag store outage must not switch
  features on or off. Before the first load, the code defaults apply, so choose them carefully. See
  [caching strategies](/posts/caching-strategies) for the same idea in general.
- **Evaluate once per request** and pass the values down, so a mid-request change cannot mix old and
  new behaviour.
- **Log which flags were on.** Add them to logs and traces, so when errors rise you can see whether
  flagged traffic is the cause (see [observability](/posts/observability-logs-metrics-traces)).
- **Treat a flag change as a production change.** Change flags through an admin page that writes the
  audit row in the same transaction.

## Testing flag combinations

Ten boolean flags give 2¹⁰ = 1,024 combinations. You cannot test them all. Instead:

- **Test both sides of every flag you touch.** Inject the flag store instead of reading a global, so
  a test can run the code with the flag on and off.
- **In CI, test the configurations that will really exist:** the current production values, plus the
  values after your planned change. Some teams also run with all flags on, to find conflicts early.
- **Avoid flags that depend on other flags.** Hidden states multiply, and bugs hide there.
- **Exercise kill switches** in staging or in a planned production exercise. Code that nobody runs
  breaks quietly.

## Flag debt: every flag is code you must delete

A flag that stays forever has a cost: two code paths that must both work, and an unused one that
slowly breaks. An old flag with a vague name is also a trap, as the next section shows.

1. **Give every flag an owner and a remove-by date**, and open the removal ticket on day one.
2. **Remove release flags soon after 100%** (say, after two quiet weeks): delete the `if`, the old
   code path and its tests.
3. **Delete in the right order:** first deploy code that no longer reads the flag, *then* delete the
   row. If you delete the row first, running code falls back to the default (`off`), and the feature
   disappears for everyone.
4. **Find stale flags automatically.** A flag nobody has evaluated for 30 days, or one stuck at 0%
   or 100%, is a candidate for removal.
5. **Never reuse a flag** for a new purpose. Names are free.

## A cautionary tale: Knight Capital, 2012

On 1 August 2012, Knight Capital, a large US trading firm, lost about $460 million because of
roughly 45 minutes of runaway automated trading. The U.S. Securities and Exchange Commission (SEC)
described what happened in an October 2013 order. According to the SEC's findings:

- Knight's automated order router still contained code for an old feature called **Power Peg**.
  Knight had stopped using Power Peg in 2003, but the code remained and could still be activated by a
  flag. A 2005 change moved the part of the code that tracked how many shares had been filled, and
  Power Peg was not retested afterwards.
- For a new NYSE program starting on 1 August 2012, Knight wrote new code to replace Power Peg. The
  new code **reused the flag** that used to activate Power Peg.
- A technician copied the new code to the servers. **One of the eight servers did not get it**, and
  no second technician reviewed the deployment.
- When trading began, orders carrying the reused flag that reached the eighth server ran the old Power
  Peg code. Without working fill tracking, it kept sending orders to the market.
- While trying to stop the problem, staff removed the new code from the seven correct servers. That
  made things worse: those servers now also ran Power Peg for flagged orders.
- In about 45 minutes, this produced about 4 million executions in 154 stocks. The SEC charged
  Knight with violating its market access rule, and Knight agreed to pay a $12 million penalty.

Lessons for anyone who uses flags (our conclusions, not the SEC's):

- **A flag's meaning must never change.** Old code on a forgotten server still believes the old one.
- **Delete dead code.** If Power Peg had been removed, the flag would have had nothing to activate.
- **Old and new versions run side by side during a deploy** (see
  [deployment strategies](/posts/deployment-strategies-and-zero-downtime-migrations)). Turn a flag on
  only after checking that *every* instance runs a version that understands it.
- **Rollback is not automatically safe.** Ask what the *old* code does with the *current* flag values.

Hindsight makes this look obvious. A blameless review (see
[incident response and postmortems](/posts/incident-response-and-postmortems)) looks for the gaps in
the system that allowed the mistake, not for a person to blame.

## Tools: build, buy, and the OpenFeature standard

A home-made flag table works well for a few services and a few flags. Consider a dedicated tool when
you have many services in several languages, non-engineers who need a UI, or experiments.

- **LaunchDarkly** is a commercial hosted service with SDKs for many languages.
- **Unleash** is open source; host it yourself or use the hosted version.
- **Flagsmith** is also open source, self-hosted or hosted, and adds remote configuration values.

**OpenFeature** is a vendor-neutral standard API for flag evaluation, and a CNCF (Cloud Native
Computing Foundation) project. Your code calls the OpenFeature API. A **provider** connects it to a
vendor or to your own flag store. To change vendors, you change the provider, not every `if`. Its
in-memory provider is handy in tests:

```python
from openfeature import api
from openfeature.evaluation_context import EvaluationContext
from openfeature.provider.in_memory_provider import InMemoryFlag, InMemoryProvider

api.set_provider(InMemoryProvider({          # in production: your vendor's provider
    "new-checkout": InMemoryFlag("on", {"on": True, "off": False}),
}))
client = api.get_client()
ctx = EvaluationContext(targeting_key="user-42", attributes={"plan": "pro"})
use_new = client.get_boolean_value("new-checkout", False, ctx)   # False = default
```

## Common mistakes

- Choosing users with `random()` instead of a stable hash.
- A default that is dangerous when the flag store is unreachable.
- Trusting a client-side flag to protect data or prices.
- Using a flag to "undo" a schema change. Use expand/contract migrations instead.
- Putting a security fix behind a flag: the "off" state is still vulnerable.
- Reusing an old flag name for new behaviour.

## Further reading

- Pete Hodgson on martinfowler.com: [Feature Toggles (aka Feature Flags)](https://martinfowler.com/articles/feature-toggles.html)
- [OpenFeature](https://openfeature.dev/): the specification, SDKs and providers
- Unleash docs: [Feature flag types](https://docs.getunleash.io/reference/feature-toggle-types) and their expected lifetimes
- SEC: [Order in the matter of Knight Capital Americas LLC (2013)](https://www.sec.gov/files/litigation/admin/2013/34-70694.pdf)
- Ron Kohavi, Diane Tang and Ya Xu, *Trustworthy Online Controlled Experiments* (book)
