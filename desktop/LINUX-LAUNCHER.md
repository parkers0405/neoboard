# Linux launcher

Extract the tarball in a permanent location. Add the extracted `neoboard` directory to your desktop session's `PATH` so the launcher can find `neochat-desktop` and its companions. From the sibling `neoboard-desktop-integration` directory, install the launcher and icon for the current user:

```sh
share=${XDG_DATA_HOME:-"$HOME/.local/share"}
install -Dm644 applications/ai.neoism.neochat.desktop "$share/applications/ai.neoism.neochat.desktop"
install -Dm644 icons/hicolor/256x256/apps/ai.neoism.neochat.png "$share/icons/hicolor/256x256/apps/ai.neoism.neochat.png"
```

Look for **NeoBoard** in the application menu. To remove the integration, delete those two installed files.