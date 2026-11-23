# Asynchronous operations

Three tools start work that takes a while: `musaic_scan_start`, `musaic_checks_start` and `musaic_analysis_start`. Each returns a handle with an operation id. They may also wait for the person to approve them, depending on the pairing's policy.

## With Tasks

When your host negotiated the Tasks extension, a started operation is returned as a Task. Poll it with the host's task calls at the interval it gives, answer any input request it carries, and cancel it with the host's cancel call if you no longer need it. The Task ends in a completed, failed or cancelled state; a completed Task carries the operation's own final record.

## Without Tasks

Poll `musaic_operation_get` with the operation id. Wait at least the suggested interval between polls. Stop when the state is final (`succeeded`, `failed`, `cancelled`, `denied` or `expired`). To stop an operation you started, call `musaic_operation_cancel`; it is honoured between items.

## Waiting for approval

A state of `awaiting_approval` means the person must decide in Musaic. Do not poll faster and do not start it again; a second start with the same input joins the first.

## Never

Do not start a scan to find out whether work exists; use the work list.
