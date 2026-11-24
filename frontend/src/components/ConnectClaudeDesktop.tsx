import * as Dialog from "@radix-ui/react-dialog";
import { useRef, useState } from "react";

import { useReturnFocus } from "@/components/DetailSheet/useReturnFocus";
import { useClaudeDesktopStatus, useConnectClaudeDesktop } from "@/hooks/useAgents";
import type { ClaudeCommand, ClaudeDesktopStatus } from "@/lib/api";
import { CLAUDE_COPY, commandLine } from "@/lib/agentCopy";
import { canConnectClaude } from "@/lib/desktop";
import { errorText } from "@/lib/errors";

const BUTTON =
  "h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition disabled:opacity-60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent";

/** One command in a block, with a control to copy it. */
function CommandBlock({ command, label }: { command: ClaudeCommand; label: string }) {
  const [copied, setCopied] = useState(false);
  const text = commandLine(command);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };
  return (
    <div className="space-y-1.5">
      <pre aria-label={label} className="mono text-xs bg-panel rounded border border-line px-2 py-1.5 whitespace-pre-wrap break-all select-all">
        {text}
      </pre>
      <button type="button" onClick={() => void copy()} className={BUTTON}>
        {copied ? "Copied" : "Copy command"}
      </button>
    </div>
  );
}

/** What the extension's folder says, in words. Nothing at all when it was not
 * found: that is not proof that no extension is installed. */
function ExtensionNotes({ s }: { s: ClaudeDesktopStatus }) {
  if (s.extension !== "detected") return null;
  const both = s.configEntry === "ours" || s.configEntry === "foreign";
  // Reinstalling is offered only where the file can be opened from here.
  const reinstall = s.route === "bundle" ? ` ${CLAUDE_COPY.extensionReinstall}` : "";
  return (
    <div className="space-y-1.5 text-sm text-fg-2">
      <p>
        {CLAUDE_COPY.extensionDetected}
        {reinstall}
      </p>
      {both && <p>{CLAUDE_COPY.extensionAndEntry}</p>}
    </div>
  );
}

/** The sentence for why the one-click route is closed, with whatever
 * command the person can use instead. Null when the route is open. */
function Unavailable({ s }: { s: ClaudeDesktopStatus }) {
  const parts: React.ReactNode[] = [];
  const setup = s.setupCommand && s.configEntry === "absent" && s.route !== "bundle" ? s.setupCommand : null;
  if (!s.bundleSupported) {
    // "Use the command below" only when there is one below.
    parts.push(
      <p key="linux">
        {CLAUDE_COPY.linux}
        {setup ? ` ${CLAUDE_COPY.linuxUseBelow}` : ""}
      </p>,
    );
  }
  if (!s.claudeFound) {
    parts.push(<p key="nf">{CLAUDE_COPY.notFound}</p>);
  } else if (s.configEntry === "ours") {
    // Where there is no extension there is nothing to switch to.
    parts.push(
      <div key="ours" className="space-y-1.5">
        <p>
          {CLAUDE_COPY.ownEntry}
          {s.bundleSupported ? ` ${CLAUDE_COPY.ownEntrySwitch}` : ""}
        </p>
        {s.bundleSupported && s.uninstallCommand && (
          <CommandBlock command={s.uninstallCommand} label="Remove the configuration entry" />
        )}
      </div>,
    );
  } else if (s.configEntry === "foreign") {
    parts.push(<p key="foreign">{CLAUDE_COPY.foreignEntry}</p>);
  } else if (s.configEntry === "unreadable") {
    parts.push(<p key="unreadable">{CLAUDE_COPY.unreadable}</p>);
  } else if (s.bundleSupported && s.cli.state === "missing") {
    parts.push(<p key="missing">{CLAUDE_COPY.programMissing}</p>);
  } else if (s.bundleSupported && s.cli.state === "unstable") {
    parts.push(<p key="unstable">{CLAUDE_COPY.unstable}</p>);
  }
  if (setup) {
    parts.push(
      <div key="setup" className="space-y-1.5">
        <p>{CLAUDE_COPY.setupInstead}</p>
        <CommandBlock command={setup} label="Set up through the configuration file" />
      </div>,
    );
  }
  return <div className="space-y-2.5 text-sm text-fg-2">{parts}</div>;
}

/**
 * Connect Claude Desktop: the readiness report, the exact program the
 * extension will run, and one confirm button. Nothing is created until the
 * button is pressed, and only the desktop window can press it.
 */
export default function ConnectClaudeDesktop({ onClose }: { onClose: () => void }) {
  const status = useClaudeDesktopStatus(true);
  const connect = useConnectClaudeDesktop();
  const returnFocus = useReturnFocus(true);
  const closeRef = useRef<HTMLButtonElement>(null);
  // The mutation's pending flag lands a render later; this stops a second click in between.
  const sent = useRef(false);
  const confirm = () => {
    if (sent.current) return;
    sent.current = true;
    connect.mutate(undefined, { onSettled: () => (sent.current = false) });
  };
  const s = status.data;
  const inShell = canConnectClaude();
  const result = connect.data;

  return (
    <Dialog.Root
      open
      onOpenChange={(o) => {
        if (!o) onClose();
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed top-1/2 left-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(520px,95vw)] max-h-[90vh] overflow-y-auto bg-panel border border-line rounded-md shadow-sheet outline-none"
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            closeRef.current?.focus();
          }}
          onCloseAutoFocus={returnFocus}
          aria-describedby={undefined}
        >
          <div className="px-4 py-3 border-b border-line">
            <Dialog.Title className="font-semibold text-md">{CLAUDE_COPY.connectAction}</Dialog.Title>
          </div>
          <div className="px-4 py-3.5 space-y-3">
            {status.isPending ? (
              <p className="text-sm text-fg-3">Checking this computer…</p>
            ) : status.isError || !s ? (
              <p role="alert" className="text-sm text-err">
                {errorText(status.error, "Could not check this computer")}
              </p>
            ) : s.route === "bundle" && s.cli.state === "found" ? (
              <>
                <ExtensionNotes s={s} />
                <p className="text-sm text-fg-2">{CLAUDE_COPY.dialogLead}</p>
                <p className="text-sm text-fg-2">{CLAUDE_COPY.commandLabel}</p>
                <CommandBlock command={s.cli.command} label="The program the extension runs" />
                <p className="text-xs text-fg-3">{CLAUDE_COPY.unsignedNote}</p>
                {!inShell && <p className="text-sm text-fg-2">{CLAUDE_COPY.outsideShell}</p>}
              </>
            ) : (
              <>
                <ExtensionNotes s={s} />
                <Unavailable s={s} />
              </>
            )}
            {connect.isError && (
              <p role="alert" className="text-sm text-err">
                {CLAUDE_COPY.openFailed} {errorText(connect.error, "")}
              </p>
            )}
            {result &&
              (result.opened ? (
                <p role="status" className="text-sm text-fg">
                  {CLAUDE_COPY.opened}
                </p>
              ) : (
                <p role="alert" className="text-sm text-err">
                  {CLAUDE_COPY.openFailed} {result.message}
                </p>
              ))}
          </div>
          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <button ref={closeRef} type="button" onClick={onClose} className={BUTTON}>
              Close
            </button>
            {s?.route === "bundle" && inShell && !result?.opened && (
              <button
                type="button"
                disabled={connect.isPending}
                onClick={confirm}
                className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
              >
                {connect.isPending ? "Opening…" : "Create and open the extension"}
              </button>
            )}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
