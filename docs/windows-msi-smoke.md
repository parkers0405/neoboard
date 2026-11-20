# Windows MSI Finish smoke test

`scripts/build-msi.ps1` calls `scripts/verify-windows-msi.ps1` before it publishes an MSI to `dist`. The verifier checks the linked MSI tables and runs `/qn` install, repair and uninstall. Those checks do **not** click Finish or inspect a real desktop process token.

Use a disposable Windows x64 VM with Explorer running, no NeoBoard installation or processes, and network disconnected. Sign in as a **standard user**, with a different administrator account available for UAC. Start the MSI from that user's Explorer, not from an elevated terminal or service. Record the user (`whoami`), Windows version and MSI SHA-256 (`Get-FileHash .\NeoBoard-x86_64.msi`).

| Case | What to verify |
| --- | --- |
| Fresh install | Approve UAC using the administrator account. Wait on the success dialog: **Launch NeoBoard** is checked and no NeoBoard GUI is running yet. Click **Finish** without changing the box. Exactly one GUI should open. |
| Identity and target | In Task Manager → Details, enable **User name**, **Elevated** and **Image path name**. The new process must be `%ProgramFiles%\NeoBoard\neochat-desktop.exe`, owned by the signed-in standard user, with **Elevated = No**, not the administrator who approved UAC. Record those columns and close the app. |
| Maintenance | Reopen the MSI, choose Repair, and finish. Reopen again, choose Remove, and finish. Neither operation should start the GUI. |
| Opt-out | Install again, clear **Launch NeoBoard** on the success dialog and click **Finish**. No GUI should start. Uninstall to clean up. |
| Cancel | Start a fresh install, cancel before installation and dismiss the cancellation dialog. No GUI should start. |

Report the artifact hash, observed results and any skipped cases separately from the automated verifier result. A hosted runner without an interactive desktop cannot establish the identity/Finish result.