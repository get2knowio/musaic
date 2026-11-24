# Contract: wording

All new interface text lives in `frontend/src/lib/agentCopy.ts`. Sentences may be polished in implementation; the meaning and the distinctions are fixed.

| Where | Text |
| --- | --- |
| Action | Connect Claude Desktop |
| Action | Save the Musaic skill for Claude |
| Dialog lead | Musaic will create an extension file for Claude Desktop and open it. Claude Desktop then asks whether to install it. Nothing is installed until you choose Install there. |
| Command label | Claude Desktop will run this program on this computer: |
| Unsigned note | Claude Desktop may say the extension's publisher is not verified. Musaic's extension is not signed. (Replaced by the observed wording once recorded.) |
| After opening | The extension file was opened. Finish in Claude Desktop, then add an assistant here if you have not yet. |
| Open failed | Musaic could not open the extension file. (with the reason) |
| Claude not found | Claude Desktop was not found on this computer. Install it, then come back here. |
| Linux | Claude Desktop extensions are not available on this platform. Use the command below instead. |
| Own entry | Claude Desktop is already connected to Musaic through its configuration file. To switch to the extension, remove that entry first: (command) |
| Foreign entry | Claude Desktop's configuration file already has a "musaic" entry that Musaic did not write. Musaic leaves it alone. Remove it yourself before installing the extension, or keep using it. |
| Extension detected | The Musaic extension is already installed in Claude Desktop. Opening the file again reinstalls it. |
| Extension and entry | Claude Desktop is running Musaic twice: once from its configuration file and once from the extension. Remove one of them. |
| Unreadable | Musaic could not read Claude Desktop's configuration file, so it cannot tell whether Musaic is already connected. |
| Program missing | Musaic's command-line program was not found beside the app. Reinstall Musaic. |
| Unstable location | Musaic is running from a disk image or a temporary location. Move it to your Applications folder, open it from there, and connect again. |
| Outside the shell | Open Musaic's desktop app to connect with one click. |
| Skill steps | 1. Save the zip. 2. In Claude, open Settings, then the skills section. 3. Upload the zip. Musaic cannot install a skill into Claude for you. |
| Older program | Running Musaic's command-line program {cli}; the app is {app}. Connect Claude Desktop again to update it. |
| Other assistants (heading) | Not available yet |
| Other assistants (body) | The Grok app, the ChatGPT app and web, the Gemini app, and Claude on the web and on phones can only reach servers on the internet. Musaic listens only on this computer, so they cannot connect to it yet. |

CLI:

- `uninstall --host claude-desktop`, no owned entry: "No Musaic entry in Claude Desktop's configuration file. If you connected from the Musaic app, remove the Musaic extension in Claude Desktop's extension settings."
- `setup --host claude-desktop`: "If you already installed the Musaic extension from the app, remove it in Claude Desktop first; otherwise Claude would run two Musaic servers."
- `doctor`: one line for the configuration entry (none, written by Musaic, not written by Musaic, unreadable), and, when the versions differ, "musaic-cli {cli} is not the same version as the running app ({app})."
