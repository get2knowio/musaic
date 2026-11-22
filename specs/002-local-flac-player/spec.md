# Feature Specification: Local FLAC Player, Now Playing & Agent Controls

**Feature Branch**: `002-local-flac-player`

**Created**: 2026-11-20

**Status**: Draft

**Input**: User description: Notion "Prompt 4 · Local FLAC player, Now Playing UI & MCP controls" (Musaic › 7 · MCP implementation specs). Add a local FLAC player to Musaic. A listener can start an album, audition a particular track while investigating a finding, and keep listening while working elsewhere in the app. A paired agent can control the same audible session and say what is playing. Audio stays on the desktop.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Play an album or track and keep listening (Priority: P1)

A listener opens an album in the Library and presses **Play Album**, or presses **Play Track** on a row. Sound comes out of the computer's selected output device. A Now Playing bar appears at the bottom of the window. It shows the cover, title and artist, with Previous, Play/Pause, Next, elapsed time, a seek slider, the duration, volume and mute. The listener then moves to Review, opens other albums, and visits Settings. Playback continues without a break, and browsing never changes what is playing.

**Why this priority**: This is the core value of the feature: listening to the collection inside the app that maintains it. Everything else builds on one reliable playback session.

**Independent Test**: In a copied synthetic library, start a multi-disc album from Library and navigate across Library, Review and Settings. Check that playback is one uninterrupted stream in displayed disc/track order, with accurate Now Playing state.

**Acceptance Scenarios**:

1. **Given** an album with two discs, **When** the listener presses Play Album, **Then** the queue becomes that album's tracks in displayed disc/track order, the first track plays, and the Now Playing bar shows that track.
2. **Given** an album is shown, **When** the listener presses Play Track on track 4, **Then** playback starts at track 4 and continues through the rest of the album, and the earlier tracks are not queued.
3. **Given** a track is playing, **When** the listener selects another album, opens a finding, or opens Settings, **Then** playback continues and the playing track does not change.
4. **Given** nothing has been played since the app started, **When** the app is open, **Then** it is silent and the Now Playing bar is not shown.
5. **Given** a track is playing, **When** the listener clicks the track title in the bar, **Then** the playing album is revealed in the Library, and the browsing selection otherwise stays independent of playback.
6. **Given** a track is playing, **When** it ends naturally, **Then** the next queued track starts without user action.
7. **Given** a track is paused, **When** the listener seeks to a new position, **Then** the position updates and playback stays paused.
8. **Given** the last track in the queue is playing, **When** it ends, **Then** playback stops at the end of the queue, the bar shows the stopped state with the last track still identified, and pressing Play starts that track again from the beginning.
9. **Given** the first track in the queue is playing, **When** the listener presses Previous more than 3 seconds into the track, **Then** the track restarts. **When** they press it within the first 3 seconds, **Then** the track also restarts (there is no earlier track).
10. **Given** a track other than the first is playing within its first 3 seconds, **When** the listener presses Previous, **Then** the previous queued track starts. Past 3 seconds, Previous restarts the current track.

---

### User Story 2 - Manage what plays next (Priority: P2)

A listener adds albums to the queue with **Add to Queue**, then opens **Up Next** from the bar or the View menu. They see the current track and the upcoming entries in order. They can play an upcoming entry now, remove one, or clear everything upcoming.

**Why this priority**: Without a queue, a listener can play only one album at a time. With one, they can line up a session while they work.

**Independent Test**: Queue two albums, open Up Next, then play an entry now, remove one and clear the rest. Check that the order and the current track are correct after each step.

**Acceptance Scenarios**:

1. **Given** a track is playing, **When** the listener chooses Add to Queue on another album, **Then** that album's tracks are appended after the existing entries without interrupting playback, and no dialog appears.
2. **Given** nothing is loaded, **When** the listener chooses Add to Queue, **Then** the tracks are queued, nothing starts playing, and the bar appears paused at the first queued track.
3. **Given** Up Next is open, **When** the listener chooses play now on an upcoming entry, **Then** that entry starts and the entries before it are skipped.
4. **Given** Up Next is open, **When** the listener removes an upcoming entry or clears upcoming entries, **Then** the current track keeps playing and only the upcoming list changes.
5. **Given** Up Next was opened from a trigger, **When** it is closed, **Then** keyboard focus returns to that trigger.
6. **Given** the same file is queued twice, **When** the listener removes one of the two entries, **Then** only that entry is removed.

---

### User Story 3 - Audition copies while reviewing duplicates (Priority: P2)

While reviewing a duplicate-track finding, the listener presses a labelled **Preview** on each existing copy to hear which one they are evaluating. A preview temporarily replaces what is audible. When the preview ends, the listener's previous track, position and playing/paused state come back. Previewing never deletes, keeps or changes a file. Those decisions stay separate.

**Why this priority**: This connects playback to Musaic's main job, maintaining the collection. It lets a person tell duplicate copies apart by ear.

**Independent Test**: While an album plays, preview copy A, then copy B, then end the preview. Check that the original track resumes at the earlier position. Repeat from a paused state and check that it comes back paused.

**Acceptance Scenarios**:

1. **Given** an album is playing at 1:10 of track 3, **When** the listener previews copy A, **Then** copy A becomes audible and the bar shows that a preview is active and which copy it is.
2. **Given** copy A is previewing, **When** the listener previews copy B, **Then** copy B replaces copy A, and the restore point stays the original track 3 at about 1:10.
3. **Given** a preview is active, **When** the listener ends it (or closes the comparison), **Then** track 3 resumes from the saved position in the saved playing or paused state, with the queue unchanged.
4. **Given** a preview is active, **When** the listener explicitly plays an album, a track or a queue entry, **Then** that choice wins, the preview ends without restoring, and no later restore overrides it.
5. **Given** nothing was loaded before the preview, **When** the preview ends, **Then** playback stops and the bar returns to its previous (hidden or stopped) state.

---

### User Story 4 - Control playback from menus and the keyboard (Priority: P3)

A listener uses the native menus (Album → Play Album / Add to Queue, View → Show Up Next, and a Playback menu with Play/Pause, Next, Previous, Stop, seek and volume) and keyboard shortcuts. Screen-reader users hear concise announcements of the track and state, and the sliders report their values.

**Why this priority**: Desktop users expect menu and keyboard control, and the app's accessibility bar applies to every new control. Pointer users still have the full experience without it.

**Independent Test**: Using only the keyboard and menus, start an album, pause, seek, change the volume and open Up Next. With a screen reader, check the announcements and slider values.

**Acceptance Scenarios**:

1. **Given** focus is not in a text field, button or dialog, **When** the listener presses Space, **Then** playback toggles between playing and paused.
2. **Given** focus is in a text field, on a button, or inside a dialog, **When** the listener presses Space, **Then** that control's normal behaviour happens and playback does not change.
3. **Given** nothing is loaded, **When** the listener opens the Playback menu, **Then** commands that need a loaded track are disabled, following the same rules as the bar.
4. **Given** the seek or volume slider has focus, **When** the listener uses the arrow keys, **Then** the value changes in steps and assistive technology reports the new value (time as elapsed of total, volume as a percentage).
5. **Given** the track changes or playback pauses or stops, **Then** a concise status announcement is made, at most once per transition.
6. **Given** Windows or Linux (which have no native menu bar commands), **Then** every menu command has an on-screen equivalent.

---

### User Story 5 - A paired agent controls and reports playback (Priority: P3)

A user has paired an external agent with Musaic. The agent can ask what is playing and get the track, the state, the position and the queue. With the user's permission to control playback, it can play an album or track it found in the catalog, enqueue, pause, resume, seek, skip, stop, set the volume or mute, edit the queue, and start or end a preview. The desktop shows the result immediately. The agent receives catalog references only, never file paths, audio, or a way to stream audio.

**Why this priority**: This is the agent half of the feature. It depends on the shared agent tool registry and the authenticated desktop bridge from Prompt 1, so it ships after that groundwork. Desktop playback (US1–US4) is useful without it.

**Independent Test**: From a paired client, find an album, play track two, seek to 30 s and pause. Check that the desktop shows the same track and position. Resume from the bar, then confirm the agent reads "playing". Send one enqueue twice and confirm only one entry was added.

**Acceptance Scenarios**:

1. **Given** an agent paired with read-only permission, **When** it reads the player state, **Then** it gets the status (playing, paused, stopped or unavailable), the current track reference and display metadata, the position and duration in milliseconds, the volume and mute, the queue revision, and the queue entries (bounded, with a way to page).
2. **Given** an agent paired with read-only permission, **When** it tries to start, change or stop sound, **Then** the request is refused with a structured permission error and nothing audible changes.
3. **Given** an agent with control permission, **When** it plays track two of an album it found, **Then** the desktop plays that track and the response returns the resulting state.
4. **Given** an agent retries an enqueue, skip or queue edit with the same request identity, **Then** the effect happens exactly once.
5. **Given** an agent sends a queue edit based on an older queue revision, **When** the queue has since changed, **Then** the edit is refused as stale and the queue is unchanged.
6. **Given** the desktop app is not running, **When** the agent calls any player operation, **Then** it receives the bridge's "app not running" response.
7. **Given** the agent's host supports change notifications, **When** the player state changes, **Then** a notification invites the agent to re-read the state. A host without notifications can still read the state.
8. **Given** a track is many minutes long, **When** the agent issues any command, **Then** the call returns as soon as the command has taken effect and does not wait for the track to finish.

---

### Edge Cases

- **Missing album folder or file**: an album marked Missing cannot be played. Its play controls are disabled with the reason given. A queued file that vanished since it was queued (folder renamed, file trashed) is skipped with a visible notice that names the track, and playback advances. If nothing playable remains, playback stops.
- **Rename or trash while playing**: renaming or trashing the playing file's album does not crash the session or corrupt the file. Playback either continues or stops with a visible notice. Later queue entries follow the album to its new location, or are marked unavailable.
- **Unreadable or corrupt track**: decoding fails with a visible notice naming the track, the track is skipped, and Retry is offered. The app stays responsive.
- **Output device lost** (headphones unplugged, device removed): playback pauses, the bar says no output is available, and Play retries on the current default device. Recovery needs no restart.
- **Identical copies**: two files with the same recording metadata are separate queue entries and separate preview targets. Each is labelled by album and location within the album, so the listener can tell them apart.
- **Stale or late commands**: a seek, skip or preview restore that arrives after a newer explicit user choice is discarded and never overrides it.
- **Release of files**: Musaic stops holding a file once playback moves past it or stops. On Windows, a file that has finished playing can be renamed or trashed right away, and an album that is playing can be renamed after playback moves to another album.
- **Listening is read-only for the library**: playing, seeking or previewing never writes tags, starts analysis or fingerprinting, triggers a rescan, or changes findings.
- **App quit**: quitting stops sound promptly and releases the output device. The next launch starts silent with an empty queue.
- **Non-FLAC or zero-length files**: anything other than a FLAC file in the catalog is not offered for playback. A zero-length track is skipped like a corrupt one.
- **Very large queue**: queuing many albums (thousands of tracks) keeps the UI responsive. Agent reads of the queue are bounded and paged.

## Requirements *(mandatory)*

### Functional Requirements

**Playback session**

- **FR-001**: The running desktop app MUST own exactly one playback session, with one audible output on the system's selected output device. Every face (UI, native menu, agent) MUST control that same session.
- **FR-002**: Transport commands from all faces MUST be applied one at a time, in arrival order. Each MUST produce one consistent resulting state that every face observes.
- **FR-003**: The session MUST support play, pause, resume, stop, absolute seek, previous, next, automatic advance at track end, and app-local volume and mute. Volume and mute affect only Musaic's output, never the system volume.
- **FR-004**: The behaviour at the edges MUST be as stated in User Story 1. At the end of the queue, playback stops with the last track identified, and Play restarts it. Previous restarts the current track when it is past 3 seconds or is the first queued track, and otherwise goes to the previous entry. Seeking while paused stays paused.
- **FR-005**: A fresh app launch MUST start silent, with an empty queue and nothing loaded. Volume and mute MAY be remembered between launches. The queue and position are not.
- **FR-006**: Playback MUST NOT make the interface unresponsive: decoding and output run independently of UI interaction.

**Catalog references and library safety**

- **FR-007**: Albums and tracks to play MUST be resolved from the catalog and current library state, by catalog reference. A track reference MUST identify one specific file copy, so copies with identical metadata are distinguishable.
- **FR-008**: Albums marked Missing, and files that no longer exist or are no longer in the catalog, MUST NOT be played. Attempts MUST return a clear, specific outcome: not found, missing, or unreadable.
- **FR-009**: Listening MUST change only player state. It MUST NOT write tags or covers, start analysis or fingerprinting, start a scan, or create, resolve or change findings.
- **FR-010**: The player MUST stop holding a file promptly once that file is no longer playing, so renames and trash work on every supported platform, Windows included.
- **FR-011**: When a playing or queued file is renamed, trashed, unreadable or corrupt, or the output device is lost, the player MUST show a visible notice and continue or recover as described in Edge Cases. It MUST never write to the file.

**Library and inspector controls**

- **FR-012**: The album header MUST offer **Play Album** and **Add to Queue**. Each track row MUST offer a keyboard-accessible **Play Track** control.
- **FR-013**: Play Album MUST replace the queue with the album's tracks in displayed disc/track order, and start the first. Play Track MUST replace the queue with that track and the album's later tracks, and start the chosen track. Add to Queue MUST append the album's tracks without interrupting playback or confirming.
- **FR-014**: Selecting or browsing albums, opening findings, or opening an album MUST NEVER start or change playback.
- **FR-015**: The playing track's row MUST be marked with an icon and accessible text ("Now playing"), wherever that album is shown.

**Now Playing bar**

- **FR-016**: Once a track is loaded, a Now Playing bar MUST be docked along the bottom of the window. Its left part MUST show the cover thumbnail (or a fallback), the track title and the artist. The centre MUST have Previous, Play/Pause, Next, elapsed time, a seek slider and the duration. The right MUST have volume, mute and Up Next.
- **FR-017**: Clicking the title in the bar MUST reveal the playing album in the Library without changing playback.
- **FR-018**: The bar MUST reserve its own layout space. It MUST NEVER cover the inspector's Apply action, a review dialog, or any other primary action, at 720 × 480 or at larger window sizes.
- **FR-019**: In a narrow window, the bar MUST keep the title, Play/Pause, Next and Up Next visible. It MUST move volume and the other secondary controls into an expanded player view.
- **FR-020**: The bar MUST use Musaic's existing panel, divider, accent and type styles in light and dark themes. The strongest emphasis is reserved for Play/Pause and the active track.
- **FR-021**: Progress MUST update smoothly, drawn locally from the authoritative position plus elapsed time. Progress MUST NOT flood the library change-event stream. Only meaningful transitions are published: track changed, state changed, queue changed, volume changed, and errors.

**Up Next**

- **FR-022**: Up Next MUST open from the bar and from View → Show Up Next. It MUST be dismissible, MUST identify the current track, and MUST list upcoming entries in order.
- **FR-023**: Up Next MUST support play now on an upcoming entry, remove an upcoming entry, and clear all upcoming entries. Each queue entry MUST have its own identity, so duplicate files are addressed separately.
- **FR-024**: Closing Up Next MUST return focus to the control that opened it. Playback MUST continue across Library, Review and Settings.

**Preview (audition)**

- **FR-025**: A duplicate-track comparison MUST offer a labelled Preview control on each existing local copy.
- **FR-026**: Starting a preview MUST save a restore point: the current track, position, playing or paused state, and the queue. Starting another preview MUST switch the audible copy and keep the original restore point. Ending the preview MUST restore exactly the saved state.
- **FR-027**: An explicit user or agent play command during a preview MUST end the preview without restoring. A preview restore MUST never override a newer explicit choice.
- **FR-028**: Preview controls MUST be visually and functionally separate from keep/remove decisions, and MUST never change files or findings.

**Menus, keyboard and accessibility**

- **FR-029**: The native menu MUST add Album → Play Album and Add to Queue, View → Show Up Next, and a Playback menu (Play/Pause, Next, Previous, Stop, Seek Forward and Seek Back, Volume Up and Volume Down, Mute). All of these MUST come from the app's single command registry, with the same availability rules as the on-screen controls.
- **FR-030**: Space MUST toggle play/pause only when focus is not in a text field, button, slider or dialog. Keyboard shortcuts MUST exist for seeking and volume.
- **FR-031**: The seek and volume sliders MUST expose their values to assistive technology. The seek value reads as elapsed of total, and the volume as a percentage. All controls MUST show visible focus.
- **FR-032**: Track changes and play, pause or stop transitions MUST produce one concise, polite status announcement each.
- **FR-033**: Every menu command MUST have an on-screen equivalent on all supported desktop platforms.

**Operations surface**

- **FR-034**: Every player action MUST be an operation on Musaic's operations surface, with an inventory row. All faces MUST go through those operations. Player state changes MUST be published as events through the existing event pattern, so every open face stays current.

**Agent (MCP) controls** *(delivered once the shared tool registry and desktop bridge from Prompt 1 exist)*

- **FR-035**: The player operations MUST be exposed to paired agents as a `musaic_player_*` tool family, through the shared tool registry. Each tool has typed input, structured output, and effect annotations: read-only, or changes audible state.
- **FR-036**: The tool family MUST cover: get state, list queue (bounded and paged), play album, play track (with an explicit start position and queue effect), enqueue, pause, resume, stop, seek (absolute, in milliseconds), previous, next, set volume, mute/unmute, queue edits by entry identity (play now, remove upcoming, clear upcoming), preview a track, and end preview. Queue edits are checked against the queue revision.
- **FR-037**: Reading player metadata and controlling audible playback MUST be separate permissions. Permission, catalog membership, input bounds and availability MUST be enforced in the shared operations, not only in the agent face.
- **FR-038**: Agents MUST receive catalog references, display metadata and structured errors only. They MUST NEVER receive local file paths, audio data or stream locations.
- **FR-039**: Enqueue, skip and queue-edit requests MUST be safe to retry: the same request identity has its effect once. Stale requests (an old queue revision, or superseded by a newer explicit choice) MUST be refused or discarded, never applied over newer state.
- **FR-040**: When the desktop app is not running, every player tool MUST return the bridge's standard "app not running" response.
- **FR-041**: Agent calls MUST return once the command has taken effect, never wait for playback to progress. When the agent host supports change notifications, player transitions MUST be signalled as a cue to re-read the state.

### Key Entities

- **Playback session**: the single audible session of the running app. It has a status (playing, paused, stopped, unavailable), the current queue entry, a position, a volume and mute, a preview state, and a revision that increases on every change.
- **Queue**: the ordered list of queue entries, with a revision number. It holds the current entry and the upcoming ones.
- **Queue entry**: one occurrence of a track in the queue. It has a unique entry identity and refers to exactly one catalog file copy. The same file can appear more than once, as distinct entries.
- **Track reference**: the catalog reference to one specific file copy of a track, within its album. It is what faces and agents exchange, never a file path.
- **Preview**: a temporary audition of a track copy, with a restore point (queue entry, position, playing/paused state). It is invalidated by any newer explicit play choice.
- **Player event**: a published transition (track changed, state changed, queue changed, volume changed, error) carrying references and the new revision, not a position stream.
- **Player permission** (agent): read-player-metadata versus control-playback, granted at pairing.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Audible playback begins within 1 second of pressing Play Album or Play Track for a local file on the reference machine.
- **SC-002**: Across a 10-minute session that moves through Library, Review, album changes and Settings, there are zero audible interruptions that the listener did not cause.
- **SC-003**: Transitions between consecutive tracks of an album insert no more than 50 ms of extra silence beyond what is encoded in the files.
- **SC-004**: A transport change made on any one face (bar, menu or agent) is visible on every other open face within 500 ms.
- **SC-005**: Ending a preview restores the earlier track and a position within 1 second of the saved point, in the saved playing or paused state, in 100% of acceptance runs.
- **SC-006**: Repeating any enqueue, skip or queue edit with the same request identity changes the queue exactly once, in 100% of contract tests.
- **SC-007**: At 720 × 480 and at 1280 × 860, no primary action (the inspector's Apply, review dialog buttons) is covered by the Now Playing bar.
- **SC-008**: After any listening session, including previews and errors, the library's file contents, embedded tags and findings are byte-for-byte and row-for-row unchanged.
- **SC-009**: Every player control can be reached and operated by keyboard alone, and announces its name, state and value to a screen reader.
- **SC-010**: Missing files, corrupt files, device loss, a stale queue edit, and a read-only agent trying to start sound each produce a visible or structured outcome, and the session is usable again without restarting the app.
- **SC-011**: After playback leaves a file (next track, stop, or quit), that file can be renamed or trashed within 1 second on macOS, Windows and Linux.

## Assumptions

- **Dependency on Prompt 1**: the agent tool family (US5, FR-035–FR-041) depends on the shared tool registry, the CLI mapping, pairing and permissions, and the authenticated desktop bridge from Notion "Prompt 1 · Shared Tools, stdio CLI & secure desktop bridge". None of these exists in the repository yet. Desktop playback (US1–US4) is built against the current operations surface and ships independently. The agent tools are defined now and delivered once Prompt 1 lands. Durable delegation (Prompt 2) and Events (Prompt 3) are not required. Events are used only when available.
- Playback applies to the running desktop app. The standalone development server may run without an audible output and report the player as unavailable.
- Only FLAC files in the catalog are playable. Playlists, streaming services, gapless crossfade, equalisers, library-wide play history and scrobbling are out of scope.
- "Displayed disc/track order" is the order the album inspector's track list already shows.
- The 3-second Previous threshold and the remembered volume and mute are conventional defaults.
- Output goes to the system's current default output device. Choosing a device inside Musaic is out of scope.
- Testing uses the generated synthetic library, copied to a temporary location with a temporary config and in-memory keys. Deterministic tests use a simulated clock and output device. One real desktop smoke test checks audible decode, seek, pause, natural advance and shutdown. Platform and device coverage, and anything left unverified, is recorded.
