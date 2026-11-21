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
call `invalidateQueries`.
