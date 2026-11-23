# MCP: skills, events and your own agent host

Musaic gives an agent three things over MCP, all through the bundled `musaic-cli mcp`
server: **tools** (see [agents.md](agents.md) and the generated
[agent-tools.md](agent-tools.md)), a **skill** that teaches the delegation workflow, and
**events** that say when delegated work is waiting or has changed. This page is about the
last two, how a host negotiates them and what to do when it cannot.

Musaic supports **local agents**: the agent host runs on your computer, you connect it, and
it reaches Musaic through the stdio process on the same computer. Nothing here makes Musaic
reachable from the internet, and Musaic starts no agent program.

## Profiles and negotiation

The server speaks two MCP revisions. A host that sends the 2026-07-28 `_meta` on its
requests (or calls `server/discover`) is served the modern profile; a host that sends
`initialize` with `2025-11-25` is served the legacy one.

| Feature | 2026-07-28 | 2025-11-25 |
|---|---|---|
| Tools | yes | yes |
| Tasks (long operations) | when the request declares the extension | not offered |
| Skills (`skills/list`, `skills/get`) | yes (extension `io.modelcontextprotocol/skills`) | `-32601` |
| Resources (the skill's files) | yes | yes |
| Prompt `musaic_research_album` | yes | yes |
| Events (`events/list`, `events/poll`, `events/stream`) | when the request declares `io.modelcontextprotocol/events` | `-32601` |

The extensions are declared once in the server's capabilities. The handlers decide: a legacy
session, or a request that does not declare the Events extension, gets `-32601`
(method not found). `musaic_capabilities.extensions` lists what the server offers;
what your host negotiated is on the pairing in **Settings, Agents** ("reported by the host").

## The skill

The skill `musaic-work-delegation` is a short guide the agent follows: discover what it may
do, list pending work before it listens for events, get, claim, renew and report, research
with evidence, submit a typed result (or `no_match` / `needs_input`), recover without doing
anything twice, release what it cannot finish, and treat everything it reads as data, not
instructions. It lives in the repository at `skills/musaic-work-delegation/` and is embedded
in the app and the CLI; nothing is read from disk when a host asks for it.

It **grants no permission**. What an agent may do is set by its pairing and enforced by
Musaic on every call.

Three different things are easy to confuse:

- **Integrity.** `skills/list` carries every file's `sha256` digest and size, computed from
  the bytes served. A host should check them after reading. This says the bytes are the ones
  Musaic offered.
- **Activation.** Whether the host loaded the skill into the agent's context. Musaic cannot
  see this. Installing or listing a skill does not show it.
- **Consent.** Whether you allow the agent to act. That is the pairing's scope and policy,
  and the approval card for anything that changes files. A skill never changes it.

Digests are verifiable; activation and consent are not something the skill can claim.

### Ways to get the skill to an agent

1. **From the server (modern hosts).** `skills/list` lists one skill; `skills/get` takes the
   URI `skill://musaic-work-delegation/SKILL.md`; `resources/read` serves each file. The
   server does not declare `directoryRead`. A request is served only if its URI equals a
   listed URI exactly; `..`, `%`-encoded input, `\`, query strings, other schemes and
   unlisted names all answer `-32602` with the fixed message `unknown resource`.
2. **As a resource or prompt (any host).** `resources/list` and `resources/read` serve the
   same files on both profiles. The prompt `musaic_research_album` returns the guide as a
   message; an optional `album_id` argument (16 hex characters) narrows the request.
3. **Installed in the host's skills folder.**

   ```bash
   musaic-cli setup --skill --dry-run     # the plan, nothing written
   musaic-cli setup --skill               # Claude Code: ~/.claude/skills/musaic-work-delegation
   musaic-cli uninstall --skill
   ```

   Only Claude Code's personal skills folder is supported; other hosts load the skill from
   the server or are not supported yet. An install writes a record
   (`.musaic-skill.json`) listing each file's digest. A directory Musaic did not write, or
   one whose files you edited, is moved to `~/.claude/musaic-skill-backups/` first (outside
   the skills folder, so it is not loaded twice). Uninstall removes only the recorded files
   that still match. The command always says **"Installed. This does not show that the host
   has loaded the skill."** `musaic-cli doctor` reports the installed version and whether it
   matches the bundled one.

The skill's version is `metadata.version` in `SKILL.md` and is independent of the app's.
`musaic-cli catalog --check` fails when any served byte differs from the committed lock
(`skill.lock.json`) without a version change.

## Events

Events say that delegated work changed; they carry no text. Two event types
(`docs/agent-events.md` is generated from the catalog):

- `musaic.work.requested.v1`: work became available (created, requeued, released or retried).
- `musaic.work.updated.v1`: any other change of a work item's state.

Each event's `data` holds exactly `workId`, `kind`, `state`, `revision` and `libraryEpoch`.
To act, read the work with `musaic_work_get`. Delivery is **at least once**: deduplicate by
`eventId` and rely on the atomic claim, so two agents hearing the same event cannot both
claim the work.

History is kept in the database in the same transaction as the work change, for seven days.
A **cursor** is opaque and tied to the library: a cursor that does not decode, belongs to
another library (a restore or a folder change), is older than the history kept, or is older
than `maxAgeMs` is answered with `truncated: true`, no events and a fresh cursor, never an
error. After a `truncated` answer (and after every reconnect) list the queued work again
with `musaic_work_list`, then continue from the new cursor. A poll with no cursor returns no
events and the current cursor, so list work first, then poll.

| Method | What it does |
|---|---|
| `events/list` | The two event types and their schemas |
| `events/poll` | Events after a cursor; `maxEvents` 1 to 100 (default 50); returns `cursor`, `truncated`, `hasMore`, `nextPollMs` |
| `events/stream` | Keeps the request open: `notifications/events/active`, then `event`, `heartbeat` and, if the pairing is revoked, `terminated`; stops on `notifications/cancelled` |
| `events/subscribe`, `events/unsubscribe` | Webhook delivery is not offered: `-32026` |

A waiting stream or poll holds one parked call in the app (never a buffer per listener) and
no lock on your library: a scan or a tool call is not slowed by it. At most four waiting
polls per pairing and eight streams per CLI process.

Errors (the codes are provisional upstream): `-32602` invalid parameters, `-32023` unknown
event name, `-32024` pairing revoked or not allowed to read work, `-32025` too many waiting
polls or streams, `-32026` unsupported, `-32601` the method is not offered to this session.

## Connecting your own agent host

You run the agent; Musaic never starts one. Pair it in **Settings, Agents**, connect it with
`musaic-cli setup` (or add the entry by hand, see [agents.md](agents.md)), then check
`musaic-cli doctor`.

Settings, Agents shows four **separate facts** per pairing, each with the time it was last
true, and no single "ready": *Paired*, *Bridge reachable* (the pairing last made an
authenticated request), *Subscription active* (a stream is open, or the host polled for events in
the last 90 seconds) and *Last work claimed*. Beside them is what the host reported about
itself (name, version, protocol profile, which extensions it declared), shown as reported by
the host, since Musaic cannot verify it. In-memory facts reset when Musaic restarts.

Review shows **Delegated work** by state, with the holder, when it was last heard from and
when its claim lapses, **"Waiting for an agent"** for queued work while no pairing has an
active subscription, and Retry for failed or cancelled work.

Whether an agent wakes up by itself depends on the host. If yours has no events or schedule
support, use **Process pending Musaic work** in Settings, Agents: copy the instruction and
give it to the agent when you want it to work (or set up your host's own schedule or hook
yourself). Musaic does not imply that a host polls.

## What is not here

Local agents are supported. Musaic has no direct cloud connectivity: an agent that runs in a
vendor's cloud cannot reach it. A later opt-in phase could add
an outbound, authenticated connector from the desktop to a remote gateway. It is specified
here and **not enabled**: it would map the same pairing principal and scopes, keep the
desktop the only authority for changes, handle offline delivery and redact local context.
Before it ships it needs its own threat model and test plan covering transport security,
scoped account pairing, revocation, encrypted persistence, reconnect, webhook address
validation and signatures, replay protection, delivery expiry and audit.

## Compatibility notes

- The earlier Events draft's top-level `events` capability and `events/subscribe` call for
  local push are not used. The published contract declares the extension under
  `capabilities.extensions` and uses `events/stream`; `events/subscribe` is webhook-only and
  not offered.
- An event's content is `data` and its type is `name` (the earlier Musaic notes called these
  `payload` and `type`).
- Skills are fetched by the URI of `SKILL.md`, not by name.
- rmcp 3.11.0 implements Skills; Events are implemented here to the documented wire
  contract through custom requests. The `ext-events` and `ext-skills` repositories were not
  read for this release; shapes follow the published overview pages and may differ in
  detail.
- The Events error codes `-32023` to `-32026` are provisional upstream.
- Results observed on real hosts are in
  `specs/005-skills-events-agent-hosts/evidence/compatibility.md`; harness results are not
  host results.

## Migrating from the earlier notes

An agent that polled `musaic_work_list` keeps working. To use events, declare the Events
extension on 2026-07-28 requests, list work once, then poll or stream from the cursor. Work
that was already queued when Musaic was upgraded has no event until its state changes, so
always list work at start.
