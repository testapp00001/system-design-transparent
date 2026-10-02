+++
title = "DNS for backend developers: records, TTLs and why changes take time"
summary = "How a DNS lookup travels from your app to the authoritative server, which record types matter, why TTLs and caches make changes slow, and how to migrate, load balance and debug with dig safely."
tags = ["networking","devops"]
level = "beginner"
date = 2026-10-02
+++

You move your API to a new server, point the DNS record at the new IP address and shut down the old
server. Some users are fine within minutes. Others get errors for hours. A colleague says "DNS
propagation takes up to 48 hours". But nothing is being pushed anywhere. DNS is a large system of
**caches**, and your old answer is still sitting in many of them. This article explains how lookups
work, which records you need, and how to change DNS without an outage.

## A few terms first

DNS (the Domain Name System) maps names like `api.example.com` to data, most often an IP address.

- **Record**: one piece of data for a name, such as "`api.example.com` has the IPv4 address
  `203.0.113.10`". Each record has a **type** (A, MX, TXT...) and a **TTL**.
- **TTL (time to live)**: how many seconds a cache may keep a record before asking again.
- **Zone**: the records one party manages, for example everything under `example.com`. The **zone
  apex** is the zone's own name, `example.com` with no prefix (also called the "naked domain").
- **Registrar**: the company where you bought the domain. It tells the `.com` registry which servers
  answer for your zone.
- **DNS host**: the company that runs those servers; often not the registrar.

## How a lookup works

When your code calls `getaddrinfo("api.example.com")`, up to five parties take part:

1. The **stub resolver** is a small library in your operating system. It checks `/etc/hosts` and the
   local cache, then asks the resolver listed in `/etc/resolv.conf`.
2. The **recursive resolver** does the real work. Your ISP, your cloud provider or a public service
   such as `1.1.1.1` runs it. It checks its cache first; on a miss, it walks the tree from the top.
3. The **root servers** only know who runs each top-level domain (TLD), such as `.com` or `.org`.
4. The **TLD servers** know who runs each domain under them: "`example.com` is served by
   `ns1.dns-host.example`". This pointer is an NS record, also called a **delegation**.
5. The **authoritative servers** for `example.com` hold the real records and give the final answer.

```text
 stub resolver       recursive resolver              root      .com TLD     example.com
 (your machine)      (ISP, 1.1.1.1, cloud)          server      server      authoritative
      |                      |                         |           |              |
      |-- api.example.com? ->|                         |           |              |
      |                      |-- api.example.com? ---->|           |              |
      |                      |<-- ask the .com servers-|           |              |
      |                      |-- api.example.com? ---------------->|              |
      |                      |<-- ask ns1.dns-host.example --------|              |
      |                      |-- api.example.com? ------------------------------->|
      |                      |<-- A 203.0.113.10, TTL 300 ------------------------|
      |<-- 203.0.113.10 -----|
      |                      | (caches every answer for its TTL)
```

The recursive resolver caches every step, so a busy resolver almost never asks the root servers.
Most lookups are answered from a cache, and a cached answer can be out of date. Queries normally use
UDP port 53 and fall back to TCP for large answers (see [TCP vs UDP](/posts/tcp-vs-udp)).

## Record types you will actually use

| Type | What it holds | Example data | Notes |
|---|---|---|---|
| A | An IPv4 address | `203.0.113.10` | A name can have several |
| AAAA | An IPv6 address | `2001:db8::10` | Only if the server really listens on IPv6 |
| CNAME | "This name is an alias of that name" | `lb-7.cloud-host.example.` | The resolver follows it to the target's A/AAAA |
| MX | Mail servers, with a priority | `10 mx1.mail-host.example.` | Lower number is tried first. A name, not an IP |
| TXT | Free text | `"v=spf1 ..."` | Email security, domain ownership checks |
| NS | Authoritative servers for a zone | `ns1.dns-host.example.` | Must match what the registrar set at the TLD |
| CAA | Which certificate authorities may issue TLS certificates | `0 issue "letsencrypt.org"` | Authorities check it before issuing |
| SRV | Host and port of a service | `10 60 5060 sip1.example.com.` | Names look like `_sip._tcp.example.com`. Used by SIP, `mongodb+srv://`, Kubernetes; not by browsers |

You will also meet **SOA** (zone settings, including the negative-caching time below) and **PTR**
(IP to name, "reverse DNS", which matters for mail servers). The newer **HTTPS** record (RFC 9460)
can tell a browser before it connects that a site supports HTTP/3 (see
[HTTP versions](/posts/http-versions)).

A small zone in the standard zone-file format. Each line is name, TTL, class (`IN`), type and data.
`@` means the zone apex.

```text
$ORIGIN example.com.
@          3600  IN  NS     ns1.dns-host.example.
@           300  IN  A      203.0.113.10
@           300  IN  AAAA   2001:db8::10
www         300  IN  CNAME  example.com.
api          60  IN  CNAME  lb-7.cloud-host.example.
@          3600  IN  MX     10 mx1.mail-host.example.
@          3600  IN  CAA    0 issue "letsencrypt.org"
```

### CNAME and the zone apex

A CNAME has a strict rule: **a name with a CNAME can have no other records**. The zone apex must
always have SOA and NS records, so `example.com` itself **cannot be a CNAME**. This hurts, because
cloud load balancers and CDNs usually give you a host name, not a fixed IP. You can CNAME
`www.example.com` to them, but not `example.com`. Workarounds:

- **ALIAS / ANAME / CNAME flattening.** Many DNS hosts offer a special apex record: the DNS host
  finds the target's addresses itself and answers with plain A and AAAA records. Cloudflare calls
  this "CNAME flattening"; AWS Route 53 has "alias records" for AWS resources. It is not standard, so
  each provider behaves differently, and a GeoDNS target sees the DNS host's location, not the user's.
- **Redirect the apex.** Point `example.com` at a small, stable server that redirects to
  `www.example.com`, and make `www` a normal CNAME.
- **Use a static IP** if your load balancer offers one.

## TTLs and caching

The zone owner sets a TTL on each record. Caches count it down; at zero, the next query goes back
to the authoritative server. The usual TTL cache trade-offs apply (see
[caching strategies](/posts/caching-strategies)):

| TTL | Good for | Cost |
|---|---|---|
| 60–300 s | Failover, load balancer names, upcoming migrations | More queries and cache misses; more dependence on your DNS host being up |
| 3600 s (1 hour) | Normal, stable records | A change takes up to an hour to reach everyone |
| 86400 s (1 day) | NS, MX, records that almost never change | A mistake also lives for a day |

Long TTLs also protect you: if your DNS host goes down, resolvers keep serving cached answers until
they expire.

### There are more caches than you think

Between your record and the connection there can also be:

- the operating system's cache (for example `systemd-resolved` on Linux);
- the browser's cache, and maybe a different resolver if the browser uses DNS over HTTPS;
- the language runtime: the JVM has its own DNS cache (the `networkaddress.cache.ttl` setting), and
  some configurations cache answers forever;
- the reverse proxy: by default, nginx resolves a host name in `proxy_pass` when it loads its
  configuration and keeps that IP until reload, unless you set up re-resolution (see its `resolver`
  directive);
- **open connections.** DNS is only used when a connection opens. A
  [connection pool](/posts/connection-pooling) or keep-alive connection uses the old IP for its whole
  life.

Some resolvers also apply their own minimum or maximum TTL. A TTL is a hint, not a guarantee.

### Negative caching

"This name does not exist" (`NXDOMAIN`) is cached too, and so is "this name has no record of this
type". RFC 2308 says how long: the smaller of the SOA record's own TTL and its last field (called
MINIMUM). See it with `dig example.com SOA`.

The classic trap: you run `curl https://new-api.example.com` *before* creating the record. Your
resolver caches "does not exist". You create the record, and for you it still does not exist, for up
to the negative TTL. Create records first, then test.

## "Propagation" is just caches expiring

Your DNS host usually updates its authoritative servers within seconds or minutes. After that,
nothing is pushed: each cache keeps the old answer until its TTL runs out. So the worst case for a
normal record change is roughly **the old TTL**, plus clients that ignore TTLs. "Propagation checker"
websites just ask many public resolvers and show whose cache has expired.

Changing **nameservers** (moving to a new DNS host) is slower. The NS records live at the TLD and you
do not control their TTL. For `.com` it is commonly two days, which is where the "48 hours" folklore
comes from. During the switch, keep both DNS hosts serving the same records.

### A safe migration, step by step

Say `api.example.com` has a TTL of 86400 (one day) and moves to a new server.

1. **At least one old TTL before the move**, lower the TTL to 300. Caches that fetched the record
   yesterday still hold it with the one-day TTL, so you must wait the full day.
2. **Make the change.** Within about five minutes, most resolvers serve the new IP.
3. **Keep the old server running.** Watch its logs until traffic is close to zero. Clients, pools and
   caches that ignore TTLs can take days.
4. **Raise the TTL again** once you will not roll back.

> [!TIP]
> Better still, keep the DNS record pointing at a load balancer and change the load balancer's
> targets. That is instant and needs no DNS wait (see
> [deployment strategies](/posts/deployment-strategies-and-zero-downtime-migrations)).

## DNS-based load balancing, GeoDNS and failover

The authoritative server chooses what to answer, so DNS can spread traffic:

| Technique | How it works | Main limit |
|---|---|---|
| Round robin | Several A records; the order rotates | No health checks; clients usually try the first IP |
| Weighted | Each IP returned in a chosen proportion | Applies per resolver, not per user |
| GeoDNS / latency-based | The answer depends on where the query comes from | Sees the resolver's location, not always the user's |
| Failover | The DNS host health-checks servers and stops returning dead ones | Detection time + TTL + clients that ignore TTLs |

- **The unit is a resolver, not a user.** All users of one big resolver get the same cached answer,
  so load is rarely even.
- **Location is a guess.** A user in Brazil whose resolver is in the US may be sent to a US region.
  The EDNS Client Subnet extension (RFC 7871) passes part of the client's IP, but many resolvers do
  not use it.
- **Failover is slow and leaky.** Health checks must fail a few times, caches must expire, and some
  clients never re-resolve while a connection is open. Expect minutes, with a long tail.

Rule of thumb: use DNS to choose a **region** or a **load balancer**, not individual app servers (see
[load balancing](/posts/load-balancing-and-stateless-servers) and
[multi-region architecture](/posts/multi-region-and-disaster-recovery)).

## Email records: SPF, DKIM and DMARC

If your app sends email from your domain, receiving servers check three TXT records. Without them,
your password-reset emails often land in spam.

```text
example.com.                TXT  "v=spf1 include:spf.mail-host.example -all"
s1._domainkey.example.com.  TXT  "v=DKIM1; k=rsa; p=MIIBIjANBgkq...IDAQAB"
_dmarc.example.com.         TXT  "v=DMARC1; p=none; rua=mailto:dmarc-reports@example.com"
```

- **SPF** lists the servers allowed to send mail for the domain. Keep exactly **one** SPF record per
  name. SPF allows at most 10 DNS lookups (each `include:` counts), so long chains break.
- **DKIM** publishes a public key at `<selector>._domainkey.<domain>`. The sender signs each message
  and receivers verify it with this key. Your email provider gives you the value.
- **DMARC** tells receivers what to do when a message fails both checks: `p=none` (only reports),
  `quarantine` (spam folder) or `reject`. Start with `none`, read the reports, then tighten it.

## Debugging with dig

`dig` asks DNS servers directly and shows the raw answer.

```text
$ dig api.example.com          # output shortened

;; ->>HEADER<<- opcode: QUERY, status: NOERROR, id: 51334
;; ANSWER SECTION:
api.example.com.          212   IN   CNAME   lb-7.cloud-host.example.
lb-7.cloud-host.example.   42   IN   A       203.0.113.10
;; SERVER: 127.0.0.53#53(127.0.0.53) (UDP)
```

`status` is `NOERROR` (found), `NXDOMAIN` (no such name) or `SERVFAIL` (the resolver failed). The
second column is the **remaining** TTL in this resolver's cache. `SERVER` is the resolver that
answered, here the local `systemd-resolved` stub.

| Command | What it tells you |
|---|---|
| `dig +short example.com AAAA` | Just the answer. Change the type for MX, TXT, CAA... |
| `dig example.com NS` | Which servers are authoritative |
| `dig @ns1.dns-host.example api.example.com` | The truth, with no cache. Look for the `aa` flag |
| `dig @1.1.1.1 api.example.com` | What a big public resolver has cached |
| `dig +trace api.example.com` | Follows the delegations from the root down, without your resolver's cache |
| `getent hosts api.example.com` | What applications on this Linux machine see, `/etc/hosts` included |

Debug in this order: the **authoritative** server (is the record right?), a **public resolver** (is
an old copy cached?), **this machine** (`getent`; `dig` skips `/etc/hosts`), then the **application**
(runtime caches, pools).

## Common outages and mistakes

- **The domain expired.** The card on file expired, or renewal emails went to someone who left.
  Website, API and email (including password resets) break at once. Turn on auto-renew and the
  registrar lock, and monitor the expiry date.
- **Wrong records.** A typo in an IP, a deleted record, or an AAAA record for a server that does not
  listen on IPv6. Manage DNS as code ([infrastructure as code](/posts/infrastructure-as-code)) so
  changes are reviewed and reversible.
- **Editing the wrong zone.** After moving DNS hosts, someone edits records at the old one. Nothing
  changes, because the TLD points elsewhere. Check `dig example.com NS` first.
- **TTL too long.** A one-day TTL on a record you suddenly must change means up to a day of partial
  outage.
- **Dangling records.** A CNAME still points at a deleted cloud resource, such as a storage bucket.
  If someone else can claim that resource name, they serve content on your subdomain (a **subdomain
  takeover**). Delete records together with resources.
- **Forgotten CAA.** You allow one certificate authority, then move to a CDN that uses another.
  Certificates stop renewing.
- **DNSSEC left behind.** DNSSEC signs your records. Change DNS host without updating the key
  reference (the DS record) at the registrar, and validating resolvers return `SERVFAIL` for your
  whole domain.

## Checklist

- [ ] You know your registrar and DNS host; auto-renew and registrar lock are on.
- [ ] TTLs are lowered at least one old TTL before planned changes.
- [ ] Old servers stay up until their traffic drops to near zero.
- [ ] SPF, DKIM and DMARC are set for every domain that sends email.

## Further reading

- RFC 1034: [Domain Names — Concepts and Facilities](https://www.rfc-editor.org/rfc/rfc1034)
- RFC 2308: [Negative Caching of DNS Queries](https://www.rfc-editor.org/rfc/rfc2308)
- Cloudflare Learning Center: [What is DNS?](https://www.cloudflare.com/learning/dns/what-is-dns/)
- Julia Evans: [Mess With DNS](https://messwithdns.net/), a playground where you create records and
  watch real queries arrive
