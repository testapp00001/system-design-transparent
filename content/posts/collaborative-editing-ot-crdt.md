+++
title = "Collaborative editing: operational transformation vs CRDTs"
summary = "Why locking and last-write-wins break when two people type at once, how operational transformation and CRDTs keep every copy in sync, what each one costs, and why you should usually use Yjs or Automerge."
tags = ["realtime","distributed-systems"]
level = "advanced"
date = 2026-10-02
+++

Alice and Bob open the same meeting notes. Alice fixes a typo in the first line while Bob adds a
sentence at the end. Within a fraction of a second each sees the other's change, and nothing is lost.
A normal `PUT /documents/42` with the full text cannot do this. This article explains the two families of techniques that can: **operational
transformation (OT)** and **conflict-free replicated data types (CRDTs)**, plus what the server still
has to do and how to choose.

## Why the usual tools fail

Real-time editing has three requirements:

1. **Local edits are instant.** A keystroke appears at once, without waiting for the server. So each
   user edits their own copy (a **replica**), and for a short time the copies differ.
2. **Convergence.** When everyone has received all edits, all copies are identical.
3. **Intention preservation.** If Bob typed "!" after "world", it must still be after "world" when it
   reaches Alice.

The familiar approaches break at least one of these:

| Approach | What happens when two people type at once |
|---|---|
| Last-write-wins (save the whole document, newest save wins) | Bob's save does not contain Alice's fix, so her fix is silently lost. |
| Locking (one editor at a time, or per paragraph) | No conflicts, but no real collaboration. Forgotten locks need timeouts. |
| Optimistic concurrency (version number, reject stale writes) | Fine for forms. For typing, almost every keystroke is "stale". |
| Three-way merge, like `git merge` | Conflicts need a human, many times per second. |

Last-write-wins is fine when the unit of data is small: Figma has described resolving each *property*
of an object (such as a shape's colour) with last-writer-wins on its server. But text is one long
sequence of characters, and that needs something smarter.

## The running example: two inserts at the same time

The shared text is `Hello world`, with positions starting at 0. Both users edit before they receive
each other's change:

```text
start:   "Hello world"
Alice:   insert(5, ",")    ->  "Hello, world"
Bob:     insert(11, "!")   ->  "Hello world!"

Bob applies Alice's op as is:   insert(5, ",")   on "Hello world!"  ->  "Hello, world!"   ok
Alice applies Bob's op as is:   insert(11, "!")  on "Hello, world"  ->  "Hello, worl!d"   wrong
```

The copies have diverged. A position number only has meaning for one version of the document. Alice's
comma moved every later character one place right, so position 11 no longer points where Bob meant.
OT fixes this by **changing the numbers**. CRDTs fix it by **not using numbers**.

## Operational transformation: rewrite the operation

OT was introduced by Ellis and Gibbs in 1989. Its core is a **transformation function**: given two
operations made at the same time on the same version, it rewrites one so that it can be applied after
the other. For two inserts: if the other insert is at an earlier position, shift mine right by the
length of its text. So Bob's `insert(11, "!")` becomes `insert(12, "!")`. If both insert at the same
position, a fixed tie-break (such as comparing client ids) decides who goes first.

A real editor needs a transformation for every pair of operation types, including rich-text
formatting and tables. Early algorithms worked peer-to-peer, without a server, and researchers later
found cases where several of them let copies diverge.

### The central server approach

The **Jupiter** system from Xerox PARC (1995) made OT practical with a simpler design. Clients talk
only to a server. The server puts all operations into one order and numbers each new version of the
document: a **revision**. Every problem is now a conversation between just two parties. Google Wave's
published OT design built on the Jupiter approach, and Google has described Google Docs as using
operational transformation.

```text
 Alice                              server (doc at r7)                             Bob
   |                                        |                                        |
   |-(1) insert(5,",")  based on r7 ------->|<-------- insert(11,"!")  based on r7 --|
   |                                        |  (2) apply Alice's op          -> r8   |
   |                                        |  (3) Bob's op is based on r7, so       |
   |                                        |      transform vs r8: insert(12,"!")   |
   |                                        |      and apply                 -> r9   |
   |<-(4) ack r8;  r9 = insert(12,"!") -----|------ r8 = insert(5,",");  ack r9 ---->|
   |                                        |                                        |
 "Hello, world!"                     "Hello, world!"                      "Hello, world!"
```

Clients transform too: Bob's own insert is not yet acknowledged when revision 8 arrives, so he
transforms the incoming operation against it first. A common simplification: each client has at most
one batch of operations awaiting acknowledgement and buffers new keystrokes meanwhile.

The trade-off: overhead is low, and the server can reject a forbidden operation before anyone sees
it. But each document needs exactly one ordering point, so all its clients must reach the same server
process, and a client returning after a day offline must transform against everything since.

Libraries in this family include ShareDB (Node.js). The ProseMirror and CodeMirror editors ship
collaboration modules with a similar central-authority design.

## CRDTs: data that merges itself

A **CRDT** (conflict-free replicated data type) is a data structure designed so that replicas can be
changed independently and merged later, in any order, and always end up identical. No server is needed
to choose an order. The standard formal description is by Shapiro, Preguiça, Baquero and Zawirski
(2011).

### State-based vs operation-based

A merge is **commutative** if order does not matter, **associative** if grouping does not matter, and
**idempotent** if applying something twice equals applying it once.

| | State-based | Operation-based |
|---|---|---|
| Replicas send | Whole state, or a "delta" (the recent part) | Each operation |
| Merge rule | `merge` is commutative, associative, idempotent | Concurrent operations commute |
| Network needs | Few: lost, repeated or reordered messages are fine | Each op exactly once, after the ops it depends on |

A simple state-based example is a counter where each server increments only its own entry
(`{"eu": 3, "us": 5}`). Merging takes the maximum per entry, and the value is the sum. Text libraries
mix the styles: Yjs sends small updates, but its documentation states they are commutative and
idempotent.

### Sequence CRDTs: give every character an identity

For text, the trick is to stop using positions. Every inserted character gets a **unique id**,
typically `(client id, counter)`. An insert says "put this *after the character with id X*", not "at
position 5". A deleted character is only marked as deleted (a **tombstone**), because someone may be
inserting right after it at the same moment.

```text
start   char:  H    e    l    l    o    _    w    o    r    l    d          (_ = space)
        id:    s1   s2   s3   s4   s5   s6   s7   s8   s9   s10  s11

Alice: insert "," with id A1 after s5          Bob: insert "!" with id B1 after s11

merged  char:  H    e    l    l    o    ,    _    w    o    r    l    d    !
        id:    s1   s2   s3   s4   s5   A1   s6   s7   s8   s9   s10  s11  B1
```

Each replica finds `s5` or `s11` wherever it is now. No number changes, and arrival order does not
matter. If both users insert after the *same* character, a deterministic rule (such as comparing ids)
orders them identically everywhere. Real algorithms such as RGA and YATA (Yjs uses a modified YATA)
add more rules and much more compact storage, but the core idea is the same.

### Yjs and Automerge

Do not implement this yourself. Two mature open-source libraries do it:

- **Yjs** (JavaScript, with a Rust port called Yrs): shared types such as `Y.Text` and `Y.Map`,
  bindings for editors like ProseMirror/Tiptap, CodeMirror and Monaco, and "providers" that sync over
  WebSocket or WebRTC, or save to IndexedDB.
- **Automerge** (Rust core, JavaScript and other bindings): a JSON-like document that keeps its full
  change history, aimed at local-first apps. `automerge-repo` handles storage and networking.

The running example in Yjs:

```js
import * as Y from 'yjs'

const alice = new Y.Doc()
const bob = new Y.Doc()
alice.getText('body').insert(0, 'Hello world')
Y.applyUpdate(bob, Y.encodeStateAsUpdate(alice))   // same starting text

alice.getText('body').insert(5, ',')                 // concurrent edits
bob.getText('body').insert(11, '!')

// Each side sends only what the other is missing (a state vector = what I have seen)
Y.applyUpdate(bob, Y.encodeStateAsUpdate(alice, Y.encodeStateVector(bob)))
Y.applyUpdate(alice, Y.encodeStateAsUpdate(bob, Y.encodeStateVector(alice)))

alice.getText('body').toString()  // "Hello, world!"
bob.getText('body').toString()    // "Hello, world!"
```

In a real app you listen to `doc.on('update', ...)` and send each update to the server. The state
vector maps each client id to the highest counter seen from it, a close relative of the vector clocks
in [time and ordering](/posts/ids-clocks-and-ordering).

## The price: metadata and tombstones

A CRDT document carries more than the visible text: ids, neighbour references and tombstones.
Libraries reduce this a lot. Yjs merges a run of characters typed by one user into a single item, and
for deleted text it keeps only a small record of the range, not the content (unless you turn garbage
collection off, for example to keep old versions). Automerge stores its full history in a compressed
binary format.

Why not remove tombstones completely? A laptop that was offline for a month may still send "insert
after character X". Removing X safely requires knowing that *every* replica has seen the deletion,
which you usually cannot know.

OT carries less weight: the server keeps a window of recent operations, and a client that falls too
far behind reloads. Either way, measure real documents after weeks of real use, not on day one.

## Offline-first editing

Because merges work in any order, a CRDT client can keep editing with no connection. It stores updates
locally (for example with `y-indexeddb`) and exchanges the missing ones on reconnect. Ink & Switch call
this style **local-first software**. Two cautions:

- **Converged is not the same as meaningful.** If two people rewrite the same paragraph offline, the
  result usually contains both versions. Everyone sees the *same* result, not a *good* one. Keep
  version history and show users what changed.
- **Business rules are not checked.** Rules such as "usernames are unique" or "stock never goes below
  zero" need a single place that decides (see
  [CAP and consistency models](/posts/cap-theorem-and-consistency-models)).

## Awareness: cursors are not document data

Other users' cursors, selections, names and colours are **awareness** state (also called presence).
It behaves very differently from the document:

| | Document | Awareness |
|---|---|---|
| Lifetime and storage | Forever, on disk | Only while connected, in memory |
| Conflicts | Must merge correctly | Latest value per user wins |
| Update rate | One per edit | Many per second while the cursor moves |

Keep them separate: cursors stored in the document would add permanent history on every move. Yjs has
a separate awareness protocol (in `y-protocols`): each client broadcasts a small JSON state, and others
drop it when the client disconnects or has not renewed it for 30 seconds.

A cursor still points *into* the document, and positions shift when others type. Store it as a
reference to a character id, not an integer; Yjs calls these **relative positions**. Throttle cursor
broadcasts to a few per second. For large audiences, see [presence at scale](/posts/presence-at-scale).

## What the server still does

With OT the server is the heart of the system. With CRDTs it is optional in theory, but almost every
product has one, usually reached over WebSockets (see
[polling, SSE and WebSockets](/posts/realtime-polling-sse-websockets)):

- **Relay.** Send each update to the other clients of the document. Route all connections for one
  document to one server process (for example, by consistent hashing of the document id), like the
  room servers in [scaling WebSockets](/posts/scaling-websockets-chat). OT requires this; CRDTs allow
  spreading a document over servers joined by [pub/sub](/posts/pub-sub-redis-nats), but one room per
  process is simpler.
- **Persistence.** Append each update to a log and periodically compact it into one snapshot (in Yjs,
  `Y.mergeUpdates` or re-encoding the loaded document). It is a small form of
  [event sourcing](/posts/event-sourcing-and-cqrs).
- **Authorization.** On connect, check that the user may read the document and whether they may
  write. Drop updates from read-only users and disconnect users whose access is revoked (see
  [authorization models](/posts/authorization-models)).

> [!WARNING]
> With a CRDT, the client applies its change locally before the server sees it. If the server refuses
> an update, that client's copy differs from everyone else's until it reloads. So enforce permissions
> per document on connect, not per edit. If the server must approve every change, that is an argument
> for a central-authority, OT-style design.

Ready-made servers exist: the `y-websocket` server and Hocuspocus for Yjs, `automerge-repo` for
Automerge, and ShareDB for OT.

## How to choose

| | OT with a central server | CRDT (Yjs, Automerge) |
|---|---|---|
| Needs a server | Yes, one ordering point per document | No, but usually has one |
| Offline and peer-to-peer | Hard | Built in |
| Metadata overhead | Low | Higher, compressed by the library |
| Server can reject one edit | Yes | Awkward |

Rules of thumb:

- **Forms and records** (a ticket's title, status, assignee): you need neither. Use per-field updates
  with optimistic concurrency (see [transactions and isolation levels](/posts/transactions-and-isolation-levels)).
- **Real-time text in a new product**: use **Yjs** or **Automerge**, chosen by the editor integration
  and language support you need.
- **Already on ProseMirror or CodeMirror, always online, server must validate every step**: their
  built-in collaboration modules are a reasonable choice.
- **Do not write your own OT or sequence CRDT.** The core idea fits in a blog post; the edge cases
  (rich text, undo, large documents, performance) took experts years.

## Common mistakes

- **Replacing the whole text on every change.** A `<textarea>` binding that deletes everything and
  inserts the new value turns each keystroke into "delete all, insert all", and others' concurrent
  edits get lost or duplicated. Use an editor binding that sends precise inserts and deletes.
- **Saving the document twice.** A CRDT *plus* a `PUT` of the full JSON brings last-write-wins back.
  Keep one source of truth.
- **Never compacting the update log**, so load times grow with history.
- **One shared undo stack.** Undo should undo *my* last change, not Bob's. Yjs's `Y.UndoManager` can
  track only changes from a given origin.

## Further reading

- [crdt.tech](https://crdt.tech/): a collection of CRDT papers, talks and implementations
- [Yjs documentation](https://docs.yjs.dev/)
- [Automerge](https://automerge.org/)
- Ink & Switch: [Local-first software](https://www.inkandswitch.com/local-first/)
- Figma: [How Figma's multiplayer technology works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/)
- Papers and books (search by title): Ellis and Gibbs, *Concurrency Control in Groupware Systems*
  (1989); Nichols et al., *High-Latency, Low-Bandwidth Windowing in the Jupiter Collaboration System*
  (1995); Shapiro et al., *Conflict-free Replicated Data Types* (2011); Martin Kleppmann, *Designing
  Data-Intensive Applications*, the chapter on replication.
