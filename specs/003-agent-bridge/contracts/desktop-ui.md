# Contract: Desktop UI

## Settings → Agents (`#agents`, added to `SETTINGS_SECTIONS`)

- **Empty state**: "No assistants are connected. Pair one to let it read your library and
  suggest fixes."
- **Pair an assistant**: a form with a name field and scope checkboxes for the 7 kinds.
  Read and propose are checked by default; the rest are unchecked. After pairing, the
  section shows how to connect: the `musaic-cli setup` command and the manual entry. The
  secret is never shown.
- **Pairing list**: each row shows name, scopes, created date, last used date, a policy
  selector per kind (Allow / Ask / Refuse, disabled for kinds outside the scope), a
  Revoke button with a confirmation, and an expandable activity list (last 100).
- **Recent changes** (FR-055): per pairing, revoked ones included, an expandable list of
  the changes it applied (newest first, at most 20): time, album, what it did, state. Each
  change has an Undo button, disabled with the backend's reason beside it (and as its
  description) when Undo would not run; an undo's own record has none. Undo opens a
  confirmation that starts on Cancel: "Put back the tags, names and cover this assistant
  changed on <Album>?" A refusal (for example, the album changed since) is shown as a
  toast with the reason, listing what changed.
- **Note shown on the page**: "Anyone who can run programs as you on this computer can use
  a pairing. Revoke pairings you no longer use."

## Review inbox

- **New item kind `approval`**: built from `useAgentApprovals()` (pending only).
- **Item text**: "<Assistant> wants to <change tags / set the cover / rename files / scan /
  run checks / analyse and write tags> for <Album>". It includes the diff from the
  proposal, which is used as a label only. It also shows the time left before expiry.
- **Actions**: Approve and Deny buttons, both keyboard operable.
- **Badge**: approvals count toward the existing single badge.
- **Opening an item** has no side effect.

## Data flow

`useAgentPairings` and `useAgentApprovals` use `staleTime: Infinity`. Both are invalidated
by `AgentStateChanged` through `useLibraryEventsStream` (rule 5). Mutation handlers do not
call `invalidateQueries`. `useAgentChanges` (`["agent", "changes", pairingId]`) also uses
`staleTime: Infinity`; it is invalidated by `AgentStateChanged(changes)` and, because
whether a change can be undone depends on the album's files, by the album events (tags,
cover, rename, missing, scan, bulk batch).
