# One-click connection to Claude Desktop

**Feature Branch**: `007-claude-desktop-bundle`
**Created**: 2026-11-24
**Status**: Draft
**Input**: Prompt 5 · One-click connection to Claude Desktop (MCP bundle)

## Overview

Connecting Musaic to Claude Desktop today means running a command in a terminal (`musaic-cli setup --host claude-desktop`), which edits Claude's configuration file. That is a poor first step for a listener who only wants their chat assistant to help with their albums.

This feature adds two actions to Settings → Agents:

- **Connect Claude Desktop** writes a small Claude Desktop extension file (an MCP bundle, `.mcpb`) that describes Musaic's own bundled command-line program, and hands that file to the operating system to open. Claude Desktop then shows its own install dialog. The listener reads it and clicks Install. Musaic never clicks for them and never changes Claude's state behind that dialog.
- **Save the Musaic skill for Claude** writes a zip of the work-delegation skill, the same bytes Musaic already serves to assistants, to a place the listener chooses, and shows the steps to upload it in Claude's skills settings. There is no documented way to install a skill into Claude Desktop from outside, so this stays a save-and-upload step and the screen says so.

Adding the assistant in Musaic (the pairing) stays as it is: after Claude has installed the extension, the listener adds or picks an assistant, and the Agents view shows it connected with the host it reported.

The scope is chat assistants only, and of those only Claude Desktop can reach a program on the listener's own computer. Coding agents and editors get no button, link or new setup target here; `setup --host claude-code` stays as it is. The other chat assistants (the Grok app, the ChatGPT app and web, the Gemini app, claude.ai on the web and on phones) reach only servers on the internet, so the Agents view and the documentation say plainly that they are not available yet.

Published vendor documentation wins over this document wherever they disagree; what was read and what was observed on a real install are recorded as evidence.

## Clarifications

### Session 2026-11-24 (documentation survey)

Read on 2026-11-24: the MCP bundle manifest specification and its schema (manifest version 0.3), the bundle tool's README and command reference, the Claude help article on local servers in Claude Desktop, and the original announcement of desktop extensions.

| Question | What the documentation says | Ruling |
| --- | --- | --- |
| May the bundle's command be a program installed outside the bundle directory? | Not stated. The schema types `command` as a plain string; the examples for interpreted servers use a bare program name found on the system; the "binary" server type is described as self-contained with a path relative to the bundle. Nothing forbids or permits an absolute path. | Unverified until tried on a real install. The design supports both shapes (FR-006 to FR-009); a recorded observation chooses which one a release uses on each platform. |
| Is an entry point required for a binary server? | The schema requires `entry_point` for every server type. | The manifest always carries one. |
| Which platforms install bundles? | macOS and Windows are named. Linux is not stated as supported. | Linux gets the configuration-file route only, and the screen says so (FR-012). |
| What does Claude Desktop show for an unsigned bundle? | Not stated. | Observe and record; the screen's text tells the listener what to expect. No signing in this feature. |
| What identifies "the same extension" when a newer file is opened, how is one removed, and where is it unpacked? | Not stated. | Observe and record. Musaic keeps the extension's name, author and identity constant across versions so a later file can replace an earlier one if the host allows it. |
| Can an installed extension and a configuration-file entry of the same name coexist? | Not stated. | Musaic never relies on it: it detects its own configuration-file entry and says which route is active (FR-013). |
| Do Claude Cowork or claude.ai on the web use locally installed extensions? | Not stated. | Cowork is tried and recorded as supported, unsupported or untested. The web and phone apps are listed as not available. |
| Variables the host substitutes in the command | `${__dirname}`, `${HOME}`, `${DESKTOP}`, `${DOCUMENTS}`, `${DOWNLOADS}`, the path separator, and `${user_config.KEY}`. | Musaic uses none but `${__dirname}` (self-contained shape only) and declares no `user_config` field at all, so nothing the listener types and nothing secret can reach the command. |

Answers from the product owner (2026-11-23), not reopened:

- If only the self-contained shape works, a second copy of the command-line program inside the bundle is acceptable, with the version checks below.
- The bundle is generated inside the app only. Release builds do not attach it as a download.

### Session 2026-11-24 (clarifications, answered without the owner; each takes the most defensible option)

- Q: The desktop shell has a folder picker but no save-file dialog, and no way to open a file with its default application. What does "the existing opener" mean here? → A: The shell already owns a small set of guarded desktop requests (native dialogs, reveal in the file manager) that take no path from the page. Connect Claude Desktop is one more of these: the page asks, the shell has the application build the bundle and opens it with the opener component it already uses. No path, argument or manifest field crosses from the page. It is offered only inside the desktop shell; the standalone development server shows the readiness report and the configuration-file route but no hand-off.
- Q: How is the skill zip saved? → A: The way the database backup is saved today: the page downloads it from the application and the shell's download handling places it where the listener's system saves downloads or asks. No new dialog is added. FR-022 and User Story 3 amended.
- Q: Where is the reported program version kept? → A: With the live session facts for a connected assistant (connected, listening), in memory. It describes a running connection, so it needs no stored column and no schema change. FR-025.
- Q: The Settings section is titled "Assistants" in the app, while the request says "Settings → Agents". → A: The app's wording stays: the section is Assistants. This document says "Agents view" for that section.
- Q: How does the desktop tell whose configuration entry it found without duplicating the command-line program's rule? → A: One shared, read-only definition of where Claude Desktop's configuration file is and of what makes a Musaic entry "written by Musaic", used by both the command-line program and the desktop. FR-013, FR-014.
- Q: Is the one-click route offered when Musaic runs from a disk image or another location that will not last? → A: The preferred shape is refused there with a message to move the app first (the same reason `setup` refuses a mounted application image); the listener is not handed a bundle that stops working when the image is ejected. Edge case amended.

### Session 2026-11-24 (first observation on a real host, and an owner addition)

Observed by the owner on macOS with Claude Desktop 6.28899.6, using a hand-made bundle of the installed-program shape:

| Observation | Ruling |
| --- | --- |
| Opening the file reached Claude Desktop's install dialog, and after Install Claude Desktop started the program at its location outside the bundle directory. | The installed-program shape is the macOS choice (FR-009). Windows stays unverified. |
| A configuration-file entry named `musaic` was already present. Claude Desktop listed Musaic once and ran two servers, one per route. Removing the listed item stopped both. | Claude's own display cannot be relied on to show the double connection; FR-013 stands, and FR-034 adds the reverse check. |
| The installed extension was unpacked to a folder named `local.mcpb.musaic.musaic` inside `Claude Extensions` in Claude Desktop's support directory, and that folder disappeared when the extension was removed. | Basis of FR-034. The folder name follows the bundle's author and name, so both are fixed as "Musaic" and "musaic". The location is not documented: a missing folder means "not detected", never "not installed". |

- Q: (Owner addition 2026-11-24) Should Musaic detect an extension that is already installed? → A: Yes, read-only, from that folder. FR-034.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Connect Claude Desktop with one click (Priority: P1)

A listener has Musaic and Claude Desktop installed. In Settings → Agents they choose Connect Claude Desktop. Musaic shows exactly what the extension will run (the program and its arguments) and what will happen next. They confirm. Claude Desktop comes to the front with its install dialog for "Musaic". They click Install there. Back in Musaic they add an assistant as usual, and within moments the Agents view shows it connected, with "Claude Desktop" as the host it reported.

**Why this priority**: This is the feature. Everything else supports or guards it.

**Independent Test**: On a machine with Claude Desktop, click the action, install in Claude's dialog, add an assistant, ask Claude to list Musaic's tools and read the library summary.

**Acceptance Scenarios**:

1. **Given** Claude Desktop is installed and no Musaic entry exists in it, **When** the listener confirms Connect Claude Desktop, **Then** a bundle file is created and the operating system opens it, and Claude Desktop shows its install dialog.
2. **Given** the confirmation step is showing, **When** the listener reads the command, **Then** it is character for character the command and arguments inside the bundle that is then written.
3. **Given** the bundle was installed and an assistant was added, **When** Claude starts Musaic's server, **Then** Claude lists Musaic's tools, a read call succeeds, and the Agents view shows the assistant connected with its reported host.
4. **Given** the bundle file was handed to the operating system, **When** the hand-off has finished, **Then** Musaic removes the file it generated once it is no longer needed, and no copy remains in the library or in Musaic's configuration directory.
5. **Given** any generated bundle, **When** its contents are inspected, **Then** no pairing secret, token or key appears in the manifest, in any file, in any file name or in any environment setting.

---

### User Story 2 - A clear answer when the one-click route is not available (Priority: P1)

A listener clicks Connect Claude Desktop on a machine where it cannot work: Claude Desktop is not installed, the platform has no bundle support, Musaic's command-line program cannot be found, or Claude already has a Musaic entry. Musaic says which of these is the case, changes nothing it was not asked to change, and offers the configuration-file route where that route can work.

**Why this priority**: A button that silently does nothing, or that creates a second Musaic server in Claude, is worse than no button.

**Independent Test**: Run each state against a scratch home directory and check the message and that no file changed.

**Acceptance Scenarios**:

1. **Given** Claude Desktop is not installed, **When** the listener opens the action, **Then** it says so, offers no bundle, and names the configuration-file route for after Claude is installed.
2. **Given** the platform is Linux, **When** the listener opens the action, **Then** it says the one-click route is not available on this platform and shows the configuration-file route.
3. **Given** Claude's configuration file already holds a Musaic entry that Musaic's setup wrote, **When** the listener opens the action, **Then** it says the configuration-file route is active, and that the entry should be removed (with the exact command) before installing the extension, so Claude never runs two Musaic servers.
4. **Given** Claude's configuration file holds a Musaic entry that Musaic did not write, **When** the listener opens the action, **Then** it says a hand-written entry exists, changes nothing, and does not offer to replace it.
5. **Given** Musaic's own command-line program is missing or cannot be verified, **When** the listener opens the action, **Then** no bundle is produced and the message names the problem.

---

### User Story 3 - Save the skill for Claude (Priority: P2)

A listener wants Claude to know how to do Musaic's research work well. They choose Save the Musaic skill for Claude and get a zip, saved the way a database backup is saved. The screen lists the steps to upload it in Claude's skills settings, and says that Musaic cannot install it for them.

**Why this priority**: The connection works without it (Musaic already serves the guide to a connected assistant), but the installed skill makes Claude follow the work routine without being told.

**Independent Test**: Save the zip to a scratch folder, unpack it, compare every file's digest with the skill's published digests.

**Acceptance Scenarios**:

1. **Given** the listener chooses the action, **When** the zip is saved, **Then** it contains the skill's guide and every reference file, each byte-identical to what Musaic serves, under one top-level folder named for the skill.
2. **Given** the same app version, **When** the zip is saved twice, **Then** the two files are byte-identical.
3. **Given** the standalone development server (no desktop shell), **When** the action is used, **Then** the zip downloads the same way.

---

### User Story 4 - Knowing which assistants cannot connect yet (Priority: P3)

A listener who uses another chat assistant looks in Settings → Agents for a way to connect it. They find a short, plain list: the Grok app, the ChatGPT app and web, the Gemini app and claude.ai on the web and on phones can only reach servers on the internet, Musaic listens only on this computer, so they are not available yet. No button for them exists.

**Why this priority**: It prevents a search for something that is not there; it adds no capability.

**Independent Test**: Read the Agents view and `docs/mcp.md`.

**Acceptance Scenarios**:

1. **Given** the Agents view, **When** the listener reads the section on other assistants, **Then** each of the four is named with the one reason it cannot connect, and none has a control.
2. **Given** the documentation, **When** a reader looks for these assistants, **Then** the same list appears with the same reason.

---

### User Story 5 - An installed extension that has fallen behind (Priority: P3, self-contained shape only)

Where the bundle has to carry its own copy of the command-line program, that copy can become older than the app after an update. The listener is told: Claude's Musaic server warns that it is older than the running app, and the Agents view shows that the connected extension is out of date, with Connect Claude Desktop offered again to replace it.

**Why this priority**: It applies only where the preferred shape (one copy of the program, always the app's own) does not work.

**Independent Test**: Connect with a command-line program whose version is lower than the app's; check the warning and the Agents view.

**Acceptance Scenarios**:

1. **Given** a connected assistant whose program reports a version older than the app, **When** the Agents view is shown, **Then** that assistant is marked as running an older version, with both versions.
2. **Given** the same mismatch, **When** the program starts, **Then** it reports the mismatch through its diagnostics and keeps working unless the two cannot interoperate.

### Edge Cases

- The listener clicks the action twice quickly: one bundle, one hand-off.
- The operating system has no application registered for the bundle file type, or opening fails: Musaic reports that the file could not be opened and removes it.
- Musaic runs from a location that later moves (an app dragged to another folder, a mounted disk image): with the preferred shape the extension's command stops resolving. When Musaic is running from a disk image or a temporary location the preferred shape is refused with a message to move the app first, and the documentation says to connect again after moving the app.
- Musaic was started with a non-default configuration directory: the bundle passes that directory, and the confirmation step shows it.
- A path with spaces, quotes or non-ASCII characters in the program's location: carried exactly, as one argument, never through a shell.
- The temporary directory is not writable: reported, nothing opened.
- Claude Desktop is running and already has the Musaic extension installed: opening a new bundle is left to Claude's own dialog; Musaic does not try to detect or alter the installed extension.
- Musaic quits before the hand-off file is removed: the next start removes leftovers of its own from the temporary location.
- An agent or a web page tries to trigger or shape the bundle: there is no agent tool for it, and the action accepts no path, argument or manifest field from its caller.

## Requirements *(mandatory)*

### Functional Requirements

**The action and its confirmation**

- **FR-001**: Settings → Agents MUST offer a "Connect Claude Desktop" action and a "Save the Musaic skill for Claude" action, both in the section where assistants are added.
- **FR-002**: Before any file is created, Connect Claude Desktop MUST show a readiness report: whether Claude Desktop was found, whether the one-click route is available on this platform, whether Claude's configuration file already has a Musaic entry and whose it is, and the exact command and arguments the bundle will run.
- **FR-003**: The command shown MUST equal the command in the bundle that is then written. Both MUST come from the same computed value, and a test MUST compare them.
- **FR-004**: The bundle MUST be created only after the listener confirms in the desktop window. There MUST be no agent tool, no bridge route and no command-line subcommand that triggers the hand-off to Claude Desktop.
- **FR-005**: A second request while one hand-off is in progress MUST be refused or folded into the first.

**The bundle**

- **FR-006**: The bundle MUST be a valid MCP bundle for the documented manifest version: required fields present, an entry point, a server command with fixed arguments, a version equal to the app's version, a constant name, author and identity across versions, and Musaic's icon.
- **FR-007**: Preferred shape ("installed program"): the server command is the absolute, verified location of the command-line program that ships inside the running app, with the fixed argument that starts the MCP server, plus the configuration-directory argument only when the app itself was started with a non-default one.
- **FR-008**: Fallback shape ("self-contained"): the bundle carries a copy of that same program and runs it from the bundle's own directory. It is used on a platform only where a recorded observation shows the preferred shape does not work there.
- **FR-009**: Which shape each platform uses MUST be a single recorded decision per platform, stated in the documentation together with the observation that justifies it, not a choice offered to the listener.
- **FR-010**: Musaic MUST build the bundle only from its own resolved program location and fixed arguments. No request, tool, agent or stored setting may supply a path, an argument, an environment value or a manifest field. The manifest MUST declare no user-configurable field and no environment values.
- **FR-011**: No pairing secret, token, key or fingerprint may appear in the manifest, in any bundled file, in any file name, in an environment value or in the log. The command-line program keeps reading its credentials from the operating system's keyring. A test MUST scan every generated byte against every secret the test fixture holds.

**Availability and the other route**

- **FR-012**: When Claude Desktop is not found, when the platform has no bundle support (Linux), or when the app's command-line program cannot be located and verified, the action MUST say which, MUST create nothing, and MUST show the configuration-file route (the existing `setup --host claude-desktop` command, with the app's own program location filled in) where that route can work.
- **FR-013**: When Claude's configuration file already has a Musaic entry, the action MUST say whether Musaic's setup wrote it or someone else did, MUST state that the configuration-file route is the active one, and MUST NOT hand a bundle to Claude until the listener has seen that. For an entry Musaic wrote, it shows the exact removal command; for any other entry it changes nothing and offers no replacement.
- **FR-014**: Reading Claude's configuration file for FR-013 MUST be read-only. Musaic's desktop MUST NOT write, back up or delete anything in Claude's directories.
- **FR-015**: `musaic-cli uninstall --host claude-desktop` MUST keep removing only the configuration entry it owns. When no such entry exists it MUST say that a connection made through the extension is removed in Claude Desktop's own extension settings, and name where.
- **FR-016**: `musaic-cli doctor` and `setup --host claude-desktop` MUST mention the extension route where they describe Claude Desktop, and `setup --host claude-desktop` MUST warn that an extension installed from the app and a configuration entry would give Claude two Musaic servers.

**Detecting an installed extension**

- **FR-034**: The readiness report MUST say whether the Musaic extension is detected in Claude Desktop (detected, not detected, or unknown when Claude Desktop's directory cannot be read), by looking read-only for the extension's folder. When it is detected, the dialog says so and that opening the file again reinstalls it. When it is detected together with a Musaic configuration entry, the dialog says Claude Desktop is running Musaic twice and that one of the two should be removed. `musaic-cli uninstall --host claude-desktop`, `setup --host claude-desktop` and `doctor` use the same detection in their messages: `setup` warns before writing an entry while the extension is detected (it still writes; the listener was told), and `uninstall` says the extension is still installed and where to remove it. "Not detected" is never worded as proof that no extension is installed.

**Files and hand-off**

- **FR-017**: The bundle MUST be written to a fresh private directory under the system's temporary location, under a fixed file name, never in the library, the configuration directory or a location chosen by the caller.
- **FR-018**: The file MUST be handed to the operating system through the desktop shell's existing way of opening things, without a new channel between the web view and the shell and without starting any agent program. Handing a file to the operating system to open is the only hand-off.
- **FR-019**: Musaic MUST remove the generated file and its directory after the hand-off (once the host has had time to read it, and in any case at the next start), and when opening fails.
- **FR-020**: The outcome of the hand-off (opened, or the reason it could not be) MUST be reported to the listener in the same dialog.

**The skill export**

- **FR-021**: Save the Musaic skill for Claude MUST write a zip holding the skill's guide and every bundled reference file under one folder named for the skill, each file byte-identical to what Musaic serves, and MUST exclude anything else.
- **FR-022**: The zip MUST be saved through the same download path as the database backup, with no new dialog and no path supplied by the page. Its bytes MUST be the same for the same app version.
- **FR-023**: The screen MUST list the steps to upload the zip in Claude's skills settings and say that Musaic cannot install a skill into Claude itself.
- **FR-024**: The zip's name MUST carry the skill's version.

**Versions (self-contained shape, and useful in both)**

- **FR-025**: The command-line program MUST tell the app its version when it connects (kept with the live facts of that connection, not stored), and the Agents view MUST show, per connected assistant, when that version is older than the app's, with both versions.
- **FR-026**: On a mismatch the program MUST report it through its diagnostics (`doctor` and its start-up log line) and keep working when the two can interoperate; when they cannot, it MUST refuse with a message that says to connect again from the app.

**Other assistants and documentation**

- **FR-027**: The Agents view and `docs/mcp.md` MUST state that the Grok app, the ChatGPT app and web, the Gemini app, and claude.ai on the web and on phones are not available yet, with the reason (they reach only servers on the internet; Musaic listens only on this computer). No control is offered for them.
- **FR-028**: No button, link, plugin or setup target for a coding agent or editor is added, and `setup --host claude-code` is unchanged.
- **FR-029**: `docs/mcp.md` and `docs/agents.md` MUST describe the one-click route, the configuration-file route, how to remove each, the unsigned-bundle behaviour as observed, the chosen shape per platform, and an evidence table of what was verified with which host version and operating system, with unverified cells marked.

**Architecture and safety**

- **FR-030**: Each new action MUST be an operation on the operations surface with a row in the operations inventory; routes only parse, call one operation and render.
- **FR-031**: The feature MUST add no listener and MUST leave the loopback bind, the launch gate, the Host and Origin check, the content security policy and the navigation lock unchanged.
- **FR-032**: The interface MUST refresh from application events; no mutation handler invalidates queries.
- **FR-033**: Interface text, documentation and messages MUST describe only what this feature does on this computer.

### Key Entities

- **Connection readiness**: what Musaic found before offering the bundle: Claude Desktop present or not, platform support, the state of Claude's configuration entry (none, written by Musaic, written by someone else, unreadable), the program location and whether it verified, the bundle shape for this platform, the exact command, and warnings (running from a disk image or temporary location).
- **Bundle**: a generated file with a manifest (name, version, author, description, icon, server command and arguments, supported platforms) and, in the self-contained shape, a copy of the program. Lives only in a temporary directory, briefly.
- **Skill archive**: a zip of the served skill files, named with the skill version.
- **Reported program version**: the version a connected assistant's command-line program gave, kept with what Musaic already records about a connected host.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On macOS and on Windows with Claude Desktop installed, a listener goes from Settings → Agents to Claude listing Musaic's tools with one click in Musaic, one in Claude's dialog and the usual steps to add an assistant, without opening a terminal. Recorded with host version and operating system.
- **SC-002**: In every generated bundle and skill archive, zero bytes match any secret held by the test fixture, and the command shown equals the command written in 100% of generated bundles.
- **SC-003**: Each of the five unavailable states (Claude absent, unsupported platform, program missing, own configuration entry present, foreign configuration entry present) produces its own message and leaves every file outside Musaic's temporary directory unchanged.
- **SC-004**: Every file in the skill archive has the same digest as the published digest of the served file; the count of files matches.
- **SC-005**: After a hand-off, no generated file remains in the temporary location after the next start of the app.
- **SC-006**: A connected Claude Desktop, reached through the bundle, completes research on a synthetic ambiguous album and leaves the approval to the listener in the desktop. Cowork's result is recorded as supported, unsupported or untested.
- **SC-007**: An assistant whose program is older than the app is shown as such within one refresh of the Agents view.
- **SC-008**: All repository checks pass, and only synthetic fixtures are added.

## Assumptions

- Claude Desktop registers the bundle file type with the operating system on macOS and Windows, so opening the file reaches its install dialog. Verified on a real install before release; if it does not hold on a platform, that platform shows the configuration-file route.
- Whether Claude Desktop runs a command outside the bundle directory is not documented. The preferred shape is tried first on each platform; the fallback shape exists so the feature does not depend on the answer.
- The steps on a real Claude Desktop (installing the extension, the unsigned-bundle wording, Cowork) are carried out by the product owner on their own machine; development never touches a real Claude configuration or a real music library, and uses a scratch home directory for every automated test.
- Detecting Claude Desktop means finding its application or its configuration directory in the documented places; a missed detection only costs the listener a message and the other route.
- Signing the bundle, attaching it to releases, and any remote or tunnelled connection are out of scope. A signing pipeline, if the observed warning justifies it, is filed as follow-up work.
- The work-delegation skill, the pairing flow, the host diagnostics and `setup --host claude-desktop` exist as delivered by the earlier agent features.
