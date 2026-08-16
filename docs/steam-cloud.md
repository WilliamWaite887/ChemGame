# Steam Cloud setup

ChemGame stores save data below `%LOCALAPPDATA%\ChemGame\saves`. This is a
per-user writable location, rather than the installed game directory, and is
intended for Steam Auto-Cloud.

In Steamworks **Steam Cloud Settings**, set an appropriate per-user quota and
enable Auto-Cloud with this root path:

| Setting | Value |
| --- | --- |
| Root | `WinAppDataLocal` |
| Subdirectory | `ChemGame/saves` |
| Pattern | `*` |
| OS | Windows |
| Recursive | enabled |

The recursive rule must include each named slot, its `.bak` recovery files,
and `.integrity-key`. The key is intentionally synchronized too: a cloud save
downloaded to another computer must be verifiable before it is loaded.

Steam Auto-Cloud synchronizes the configured files on launch and exit, so it
does not require a second save implementation in the Steam networking code.
Test it with a developer account on two machines before publishing the Cloud
settings. A leftover `.tmp` file after a crash is never read as a save and can
be ignored; the signed primary and `.bak` files are the recovery sources.
