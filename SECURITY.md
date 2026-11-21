# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for security problems.

Report vulnerabilities privately through GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
on this repository's **Security** tab (**Report a vulnerability**).

Please include:

- a description of the issue and its impact,
- steps to reproduce (a minimal proof of concept if possible),
- the affected version or commit, and your OS.

We aim to acknowledge reports within a few days and will keep you updated while we
investigate and prepare a fix.

## Supported versions

Musaic is pre-1.0. Security fixes go into the latest release and `main`; there is no
long-term support branch. The earlier Python implementation (tag `python-final`) is no
longer maintained.

## How Musaic is put together

Musaic is a **local-first** desktop app. It runs on your machine, against your own music
library, and keeps its data under `~/.musaic`. The parts most relevant to a security
review:

### The local API

The desktop app runs an HTTP server inside its own process and shows the UI from it.

- It binds **`127.0.0.1` only**, on a port chosen at random at each launch.
- **Host and Origin checks** (`rust/crates/musaic-api/src/host_check.rs`): every request
  must name a loopback host (`127.0.0.1`, `localhost`, `[::1]`) in `Host`, and any
  `Origin` header must be a loopback origin. This stops DNS-rebinding pages and
  cross-site requests from other web pages in your browser.
- **Per-launch cookie** (`rust/crates/musaic-api/src/launch.rs`): the desktop window's
  first request redeems a random 256-bit token, once, for an `HttpOnly`,
  `SameSite=Strict` cookie. Every other request without that cookie is refused. This
  keeps other local users and processes that can reach the port, but not your config
  folder, out of the API.
- A strict **Content-Security-Policy** header on every response
  (`rust/crates/musaic-desktop/src/gate.rs`): the page may load and connect only to its
  own origin.
- **No IPC.** The webview is granted no Tauri capabilities and no commands. Navigation is
  locked to the app's origin (`rust/crates/musaic-desktop/src/nav.rs`): a public `https`
  link opens in your browser; plain `http`, local-network hosts and other schemes are
  refused.
- The desktop-only endpoints (native confirm and folder dialogs, "reveal in file
  manager") need the cookie, the exact Host and the app's own Origin; "reveal" takes an
  album id, never a path, so only a folder of the configured library or the app's trash
  can be shown.

The standalone `musaic-api` server binary (for development) applies the same Host/Origin
checks but no launch cookie. It binds `127.0.0.1` by default; if you bind it to another
address, anyone who can reach that address can use the API — there is no login.

### Agent access

Musaic can let an assistant you choose (Claude Desktop, Claude Code, any MCP host) read
your library and propose changes, through the bundled `musaic-cli`. The rules, in
`docs/agents.md`:

- **A separate route family**, `/__musaic/agent/v1/*`, with its own guard
  (`rust/crates/musaic-api/src/agent_guard.rs`): the peer is on the loopback interface,
  there is exactly one `Host` header equal to `127.0.0.1:<port>`, no `Origin`, no
  cross-site fetch, a JSON body of at most 256 KiB, and (except for the handshake) a
  bearer token. The launch cookie is never read there, and the existing launch gate and
  Host / Origin check are unchanged for every other route.
- **Pairing**: you create a pairing in Settings, Agents. Its 32-byte secret is generated
  by the app and stored only in the OS keyring, under the service name `musaic-agent`
  (account `<config dir id>:<pairing id>`); the database holds only its SHA-256
  fingerprint, and nothing writes it to a log, an argument, a URL or the web view. The
  secret never crosses the wire: the CLI and the app prove to each other that they hold
  it with an HMAC over fresh nonces (the app's `hello` and `exchange` routes), and the CLI
  uses the 15-minute token only after the app's own proof verifies, so a process squatting
  on the port learns nothing and gets no calls.
- **Lockout**: five failed exchanges within a minute refuse every exchange for a minute,
  then two, four and so on up to fifteen. Authorization headers are redacted from every
  log.
- **Scope and policy**: each pairing has a scope (which of seven permissions it may ever
  use) and a policy per permission (Allow, Ask, Refuse). A permission outside the scope is
  refused whatever the policy says. Anything that changes a file, and any scan, check run
  or analysis the policy says to Ask about, waits for you in the Review inbox; only you can
  approve, and an approval is revalidated against the files at the moment it runs (revision,
  expiry, pairing). A restart expires every pending request. Before an assistant's change
  writes anything, the prior values, names and cover are recorded under
  `<config>/agent-changes/` and kept 90 days.
- **No paths**: the tools return ids and bare file and folder names, never a filesystem
  path, and the contract offers no way to run SQL or a shell, read keys, delete, restore,
  change settings, or change permissions.
- **Honest limit**: a pairing authenticates whoever holds its credential, that is, any
  process running as you that can read your keyring entry. It does not identify one agent
  program or model. Revoke pairings you no longer use. On macOS the Keychain may ask once
  to let `musaic-cli` read the entry.
- **The descriptor** `<config>/agent-instance.json` holds no credential (the app's port,
  instance id and process id). The CLI refuses it if it is a symlink, not a regular file,
  owned by someone else, readable by others (Unix), or names a host other than
  `127.0.0.1`. On Windows the file relies on the per-user profile directory's inherited
  permissions rather than an explicit ACL; this is tracked as a follow-up.

### Outbound requests

Musaic talks to MusicBrainz, AcoustID and the Cover Art Archive over HTTPS (rustls).
Server certificates are verified by the operating system (`rustls-platform-verifier`:
the Security framework on macOS, the certificate store on Windows, the system CA bundle
on Linux), so the system's trust decisions, including roots an administrator added or
removed, apply. The Mozilla roots built into the app are used only when the system has no
trust store at all. Verification cannot be turned off. A refused certificate is reported
as such, naming the host only (`rust/crates/musaic-net/src/live.rs`).

**Cover downloads** (`rust/crates/musaic-net/src/preview.rs`). Every remote image Musaic
fetches — a cover URL you paste, a Cover Art Archive cover, a suggested cover — must:

- use `https`, on the default port, with no user name or password in the URL;
- name a public host: not `localhost`, `*.local`, `*.internal`, `*.home.arpa`, `*.lan`,
  a single-label name, or a loopback / private / link-local / other non-public IP
  address;
- still be public once resolved: the connection is refused if the name resolves to a
  non-public address;
- follow at most 5 redirects, each checked by the same rules;
- answer with at most 8 MiB of `image/jpeg`, `image/png`, `image/gif` or `image/webp`
  whose first bytes match that format.

Use cover upload for an image on your own machine or network.

### Secrets

API keys (for example the AcoustID key) are stored only in the **OS keyring** — macOS
Keychain, Windows Credential Manager, or the freedesktop Secret Service on Linux — under
the service name `musaic` (`rust/crates/musaic-secrets`); agent pairing secrets use the
service name `musaic-agent` (see "Agent access"). They are never written to the
database, to backups or snapshots, or to the log (`server.log` masks key values). Keys
that an older version stored in the database are moved into the keyring on first start
and then removed from the database and from the pre-upgrade backup. If no keyring is
available, Musaic stores no keys at all (there is no file fallback) and the Settings page
says so.

### Files

Musaic reads and writes FLAC files, `cover.jpg` files and folder names inside the library
folder you choose. Tag and cover data from those files is untrusted input. Deleted files
go to `~/.musaic/trash/`, not straight to deletion. A database you restore is checked before
the live one is touched, and the previous files are kept aside.

### Builds

Release builds are produced by GitHub Actions from the tagged commit. They are **not
code-signed or notarised** yet; check downloads against the release's `SHA256SUMS.txt`.
