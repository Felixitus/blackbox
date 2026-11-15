# Installation

## With the installer

Run `Blackbox_<version>_x64-setup.exe`. It installs for your own Windows
account only, into `%LOCALAPPDATA%\Blackbox`, without administrator rights.
The installer creates a Start menu entry and installs the desktop app
(`blackbox-app.exe`) and the command line tool (`cli\blackbox.exe`).

Uninstall through Windows Settings › Apps. The collected data is kept unless
you tick "Delete app data".

## Building from source

### Requirements

- Windows 10 or 11, 64-bit
- [Rust](https://rustup.rs) stable with the target `x86_64-pc-windows-msvc`
- Visual Studio 2022 (or Build Tools) with "Desktop development with C++"
  **including the Windows SDK**. Without the SDK the build fails with
  `cannot open input file 'kernel32.lib'`. Install it with:

  ```
  winget install Microsoft.WindowsSDK.10.0.26100
  ```

- [Node.js](https://nodejs.org) (only for the Tauri tooling when building the app)

### Build and test

```
cargo test
cd app
npm install
npx tauri build
```

Result:

- `target\release\bundle\nsis\Blackbox_<version>_x64-setup.exe` – installer
- `target\release\blackbox-app.exe` – desktop app
- `target\release\blackbox.exe` – command line tool

`npx tauri build --no-bundle` builds only the programs without the installer.
On the first installer build, Tauri downloads its NSIS tooling from the
official Tauri releases on GitHub.

The interface can be viewed in a browser with sample data, without a Rust
build: `node app/scripts/serve-ui.mjs`, then <http://localhost:4173>.

## Frame capture

The installer ships **PresentMon 2.6.0** by Intel (MIT license, license text
under `licenses\PresentMon-LICENSE.txt` in the install folder). Blackbox uses
it automatically; no setup is needed.

PresentMon needs access to the Windows performance events (ETW). Two ways:

- **Recommended:** in the app under Settings › "Allow frame capture
  permanently". After a Windows confirmation this adds your Windows account to
  the group "Performance Log Users". It takes effect after the next sign-in;
  from then on Blackbox also measures when started at sign-in, without
  administrator rights.
- Start Blackbox as administrator every time.

Another PresentMon version can be set in the settings or with
`blackbox set-presentmon <path>`; an own path takes precedence over the
bundled copy.

**When building**, `app/scripts/fetch-presentmon.mjs` downloads the pinned
version from the official GitHub releases and aborts if the SHA-256 checksum
does not match. The file goes to `app/src-tauri/vendor/` and is not part of
the repository. To update it, change the version and checksum in the script
and the license text under `app/src-tauri/licenses/`.

## Storage locations

| What | Where |
|---|---|
| Configuration | `%APPDATA%\Blackbox\config\config.json` |
| Database | `%LOCALAPPDATA%\Blackbox\data\blackbox.db` |
| Logs | `%LOCALAPPDATA%\Blackbox\data\logs\` (7 days) |
| Loadout Vault | `%LOCALAPPDATA%\Blackbox\data\vault\` |

`blackbox info` shows the actual paths.

## Removing a source build

Delete the built programs and the two `Blackbox` folders under `%APPDATA%`
and `%LOCALAPPDATA%`. If autostart was enabled, disable it in the settings
first. Blackbox creates nothing else.
