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
| Events (`events/list`, `events/poll`, `events/stream`) | yes (extension `io.modelcontextprotocol/events`) | `-32601` |

The extensions are declared once in the server's capabilities. The handlers decide: a legacy
session gets `-32601` (method not found) for `skills/*` and `events/*`. A host may declare
the Events extension in its per-request capabilities; Musaic serves `events/*` either way, as
the Events specification makes the client's declaration optional.
`musaic_capabilities.extensions` lists what the server offers; what your host negotiated is
on the assistant in **Settings, Assistants** ("reported by the host").

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
Version 1.1.0 teaches batches: when several `requested` events with the same `batchId`
arrive, list the work by `batchId`, claim with `musaic_work_claim_many` (up to 25 at a time)
and renew with `musaic_work_renew_many`; each item is still completed, failed or released
on its own.
`musaic-cli catalog --check` fails when any served byte differs from the committed lock
(`skill.lock.json`) without a version change.

## Events

Events say that delegated work changed; they carry no text. Two event types
(`docs/agent-events.md` is generated from the catalog):

- `musaic.work.requested.v1`: work became available (created, requeued, released or retried).
- `musaic.work.updated.v1`: any other change of a work item's state.

Each event's `data` holds `workId`, `kind`, `state`, `revision` and `libraryEpoch`, and
`batchId` when the request was made by a bulk **Fix all** run (an opaque reference, no
text). To act, read the work with `musaic_work_get`. Delivery is **at least once**: deduplicate by
`eventId` and rely on the atomic claim, so two agents hearing the same event cannot both
claim the work.

History is kept in the database in the same transaction as the work change, for seven days.
A **cursor** is opaque and tied to the library: a cursor that does not decode, belongs to
another library (a restore or a folder change), is older than the history kept, or is older
than `maxAgeMs` is answered with `truncated: true`, no events and a fresh cursor, never an
error. After a `truncated` answer (and after every reconnect) list the queued work again
with `musaic_work_list`, then continue from the new cursor. A poll with no cursor returns no
events and the current cursor, so list work first, then poll.

**Compatibility.** Since skill 1.1.0, work event payloads may carry `batchId`. The field is
optional and additive, so the event types stay `.v1`. Read the payload schema from
`events/list` rather than a cached copy: a client that validates against the earlier schema
with `additionalProperties: false` will reject events that carry it. The batch is stored
with the event's history row, so a poll returns it even after the request is cleared.

### Batches

A bulk **Fix all** run with research on makes one research request per album the resolver
could not settle, all with the same `batchId`. Two tools handle them without one call per
item:

- `musaic_work_claim_many` (`batchId` and `kind` optional, `max` 1 to 25, default 25, and an
  `idempotencyKey`) claims up to `max` eligible queued requests in one transaction, oldest
  first. Fewer than asked is not an error; a request whose album changed since it was
  queued is skipped. Retrying with the same key returns the same claims; the same key with
  a different input is `IDEMPOTENCY_CONFLICT`.
- `musaic_work_renew_many` (`leases`, 1 to 25 distinct `workId`s)
  renews each lease (its token and claim generation) as `musaic_work_renew` would, and reports `renewed` and `refused` per
  item.

`musaic_work_list` takes an optional `batchId` filter. Both tools need the same permissions
as the single claim and renew (read access and proposal submission). The single-item tools
behave as before.

| Method | What it does |
|---|---|
| `events/list` | The two event types and their schemas |
| `events/poll` | Events after a cursor; `maxEvents` 1 to 100 (default 50); returns `cursor`, `truncated`, `hasMore`, `nextPollMs` |
| `events/stream` | Keeps the request open: `notifications/events/active`, then `event`, `heartbeat` and, if the pairing is revoked, `terminated`; stops on `notifications/cancelled` |
| `events/subscribe`, `events/unsubscribe` | Webhook delivery is not offered: `-32026` |

A waiting stream or poll holds one parked call in the app (never a buffer per listener) and
no lock on your library: a scan or a tool call is not slowed by it. At most four waiting
polls per pairing and eight streams per CLI process.

Errors: `-32602` invalid parameters, `-32023` unknown event name (`data: {"kind": "event"}`),
`-32024` pairing missing, revoked or not allowed to read work, `-32025` too many waiting polls
or streams (`data.limit` `waitingPolls` or `streams`, `data.max`), `-32026` unsupported
(`data: {"feature": "deliveryMode", "value": "webhook"}`), `-32601` the method is not offered
to this session.

## Connecting your own agent host

You run the agent; Musaic never starts one. Add it in **Settings, Assistants**, connect it with
`musaic-cli setup` (or add the entry by hand, see [agents.md](agents.md)), then check
`musaic-cli doctor`.

Settings, Assistants shows four **separate facts** per assistant, each with the time it was
last true, and no single "ready": *Added*, *Connected* (the assistant last made an
authenticated request), *Listening for work* (a stream is open, or the host polled for
events in the last 90 seconds) and *Last request picked up*. Beside them is what the host reported about
itself (name, version, protocol profile, which extensions it declared), shown as reported by
the host, since Musaic cannot verify it. In-memory facts reset when Musaic restarts.

Review shows **Delegated work** by state, with the holder, when it was last heard from and
when its claim lapses, **"Waiting for an assistant"** for queued work while no pairing has an
active subscription, and Retry for failed or cancelled work.

Whether an agent wakes up by itself depends on the host. If yours has no events or schedule
support, use **Process pending Musaic work** in Settings, Assistants: copy the instruction and
give it to the agent when you want it to work (or set up your host's own schedule or hook
yourself). Musaic does not imply that a host polls.

## Claude Desktop

Two routes, one at a time. Do not use both.

**The one-click route (the Musaic app).** In **Settings, Assistants** choose **Connect Claude
Desktop**. Musaic first shows what it found (whether Claude Desktop is installed, whether its
configuration file already has a `musaic` entry and whose, and the exact program the extension
will run), then, only inside the desktop window and only after you confirm, writes an MCP bundle
(`Musaic.mcpb`: a zip with `manifest.json` and `icon.png`) to a private temporary folder and asks
the operating system to open it. Claude Desktop then shows its own install dialog; nothing is
installed until you choose Install there. The bundle declares no settings and no environment
values and holds no secret: the program it runs, `musaic-cli mcp`, reads its credentials from your
keyring as before. After installing, add an assistant in Settings as usual. The file is removed
when opening fails, after 60 minutes, and at the next start. Musaic's extension is not signed, so
Claude Desktop may say its publisher is not verified.

The one-click route is refused while Musaic runs from somewhere that will not last: a mounted
disk image, an AppImage, a translocated copy (macOS moves an app opened straight from Downloads
to a random read-only folder) or the temporary folder. Move the app first. On macOS a location
under `/Volumes/` counts as a disk image when that volume is mounted read-only (as a downloaded
disk image always is) or when its mount cannot be read. A copy on a writable external drive is
accepted: the extension works while that drive is attached, and stops when it is not.

**The configuration-file route.** `musaic-cli setup --host claude-desktop` (see
[agents.md](agents.md)). This is the only route on Linux, and the one to use when the program
cannot be found beside the app.

**Which shape the bundle has.** *Installed program*: the extension's command is the absolute
location of the `musaic-cli` that ships inside the app. *Self-contained*: the bundle carries a
copy. Both are built and tested; one constant per platform (`shape_for`) picks. Observed on macOS,
Claude Desktop 6.28899.6: Claude ran `musaic-cli mcp` at its location outside the bundle, so macOS
uses the installed program. Windows is unverified and uses the same shape until an observation
says otherwise.

**Detecting an installed extension.** The readiness report, `doctor`, `setup` and `uninstall` look,
read-only, for the folder Claude Desktop unpacks the extension into:
`Claude Extensions/local.mcpb.musaic.musaic` inside the folder that holds
`claude_desktop_config.json`. This was observed on macOS (Claude Desktop 6.28899.6; the folder
disappeared when the extension was removed) and is not a documented location; the same relative
folder is assumed on Windows and is unverified. The folder name follows the bundle's author name
and name, which Musaic fixes as "Musaic" and "musaic". A missing folder is reported as "not
detected", never as proof that nothing is installed. An installed extension does not block
installing again (opening the file reinstalls it); a detected extension together with a `musaic`
configuration entry is reported as Musaic running twice.

**The skill.** **Save the Musaic skill for Claude** downloads
`musaic-work-delegation-<version>.zip`: the skill's guide and references under one folder, the
same bytes every time. In Claude open Settings, then the skills section, and upload the zip.
Musaic cannot install a skill into Claude for you.

**Not available yet.** The Grok app, the ChatGPT app and web, the Gemini app, and Claude on the
web and on phones can only reach servers on the internet. Musaic listens only on this computer,
so they cannot connect to it yet.

What was checked, with which Claude Desktop version and system, is in
`specs/007-claude-desktop-bundle/evidence/compatibility.md`; cells not yet recorded are marked
untested.

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
- rmcp 3.11.0 implements Skills; Events are implemented here through custom requests.
- Both halves were checked field by field against the extension specifications:
  `modelcontextprotocol/ext-events` at `c77b84395eff0ae32f53b0516c2214fe276ed050`
  (`specification/beta/events/events.mdx`), and `modelcontextprotocol/ext-skills` at
  `1631b3f193c9bd90c5693743d7e0fc39faf266d3` (`specification/beta/skills.mdx`,
  `schema/beta/schema.ts`). The check is of the code and the conformance harness, not of a
  real host. Confirmed as implemented: the extension ids and settings objects; `resultType: "complete"`
  on every result; the `events/list` descriptor (`name`, `description`, `delivery`,
  `inputSchema`, `payloadSchema`); the `events/poll` request (`name`, `arguments`, `cursor`,
  `maxAgeMs`, `maxEvents`) and result (`events`, `cursor`, `truncated`, `hasMore`,
  `nextPollMs`); the occurrence fields (`eventId`, `name`, `timestamp`, `data`, `cursor`);
  the notification names `notifications/events/active`, `event`, `heartbeat` and
  `terminated`, their params and `_meta["io.modelcontextprotocol/subscriptionId"]` set to the
  request id; the granted `heartbeatMs` (default 30 000) and a heartbeat whenever it passes
  with nothing sent; cancellation by `notifications/cancelled`; the error numbers
  (`NotFound -32023`, `Forbidden -32024`, `ResourceExhausted -32025`, `Unsupported -32026`,
  now assigned, not provisional); the Skills entry (`uri`, verbatim `frontmatter`, a complete
  `resources` list of `uri`, `sha256:<hex>` `digest` and `size`), `ttlMs` and `cacheScope` on
  `skills/list` and `skills/get`, `-32602` for an unknown skill or resource, and the
  `resources` capability alongside the extension.
- Changed to match: a request that does not declare the Events extension is now served
  (the client's declaration is optional; it was refused with `-32601`); `events/poll` events
  no longer carry a per-event `cursor` (push only); the final `events/stream` result carries
  the subscription id in `_meta`; `NotFound`, `ResourceExhausted` and `Unsupported` carry
  their typed `data`; a missing pairing maps to `Forbidden`, not an internal error; the
  `SKILL.md` resource in `resources/list` is named from its frontmatter (`name`).
- Not used, as the specification allows: `notifications/events/error` (a transient failure
  is retried silently while heartbeats continue), `listChanged` (the event types are fixed),
  `nextCursor` (one page), `directoryRead`, and webhook delivery with its
  `CallbackEndpointError` (`-32027`). The Streamable HTTP routing headers do not apply to
  stdio.
- Results observed on real hosts are in
  `specs/005-skills-events-agent-hosts/evidence/compatibility.md`; harness results are not
  host results.

## Migrating from the earlier notes

An agent that polled `musaic_work_list` keeps working. To use events, send 2026-07-28
requests (declaring the Events extension is optional), list work once, then poll or stream
from the cursor. Work that was already queued when Musaic was upgraded has no event until
its state changes, so always list work at start.
