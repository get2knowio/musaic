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

### Outbound requests

Musaic talks to MusicBrainz, AcoustID and the Cover Art Archive over HTTPS (rustls with
bundled root certificates).

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
the service name `musaic` (`rust/crates/musaic-secrets`). They are never written to the
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
