# Blackbox

A flight recorder for your gaming PC. Blackbox answers the question
**"What changed since the game last ran well?"**

- Logs system changes: Windows build and updates, graphics and audio
  drivers, BIOS, power plan, gaming-related Windows settings, startup apps and
  the version of every game.
- Records the performance of every play session (average FPS, 1 % low,
  stutters per minute).
- Detects performance drops compared with earlier sessions of the same game
  and lists the changes in between, most likely first – including programs
  that used noticeable CPU time only in the slow session.
- Announces drops and possible settings resets with Windows notifications;
  the matching report can be copied for forums or support without user names
  or paths.
- **Loadout Vault:** backs up the settings files of your games whenever they
  change, detects possible resets after patches and restores earlier versions
  with one click – optionally only controls and gameplay options, leaving
  resolution and graphics untouched.
- **New PC:** export the personal settings of all games as a profile and
  apply them on the new PC.
- Detects games from Steam, Epic, GOG, Battle.net, EA app, Ubisoft Connect
  and Riot.
- German and English (follows the Windows language or can be chosen).

Blackbox injects nothing into games and changes nothing on the system. The
only write outside its own folder is restoring a game setting, and only when
you ask for it. All data stays local.

## Build

Requirements: Rust (stable, MSVC), Visual Studio 2022 with C++ tools and the
Windows SDK, Node.js.

```
cargo test
cd app && npm install && npx tauri build
```

Result: `target\release\bundle\nsis\Blackbox_<version>_x64-setup.exe`
(per-user installer, includes the app, the command line tool and PresentMon).

Documentation: [docs/INSTALL.md](docs/INSTALL.md),
[docs/USAGE.md](docs/USAGE.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
The app, the command line tool and all reports are available in English and
German.

## Third party

The installer contains [PresentMon](https://github.com/GameTechDev/PresentMon)
2.6.0 by Intel Corporation under the MIT license (license text in
[app/src-tauri/licenses/PresentMon-LICENSE.txt](app/src-tauri/licenses/PresentMon-LICENSE.txt)).

## License

Proprietary. Copyright (c) 2026 Felix Spiß, all rights reserved. Use,
copying, modification or distribution only with written permission, see
[LICENSE](LICENSE).