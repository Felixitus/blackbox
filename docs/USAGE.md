# Usage

## Desktop app

Start `blackbox-app.exe`. Blackbox immediately captures the starting point of
your system and backs up the settings of the detected games. After that:

- **Closing the window** – Blackbox keeps running in the notification area
  and keeps recording. A click on the icon opens the window again.
- **Quit** – right-click the icon in the notification area › Quit. A running
  session is closed cleanly.
- **Scan now** – captures changes right away instead of after 15 minutes.

Only one Blackbox can run at a time; a second start brings the existing
window to the front.

**Autostart:** Settings › "Start Blackbox automatically at sign-in". Blackbox
then starts without a window in the notification area. For the frame rate to
be measured there as well, choose "Allow frame capture permanently" once (see
[INSTALL.md](INSTALL.md)).

**Detected games:** Steam, Epic Games, GOG, Battle.net, EA app, Ubisoft
Connect and Riot. Where a launcher provides no version number (Ubisoft,
Riot), the modification date of the game files serves as the version.

**Language:** Settings › Language – Automatic (Windows language), Deutsch or
English. Applies to the app, notifications, reports and the notification area
menu; journal entries that are already stored appear in the new language too.

### Views

| View | Content |
|---|---|
| Game (left bar) | Figures of the last session, detected drops with suspects, history, all sessions |
| Loadout Vault | Backed-up settings files per game, differences, restore |
| Change journal | Every detected change to the system, grouped by day |
| System state | What the last scan captured |
| Settings | Language, frame capture, autostart, notifications, storage locations |

A red dot in the left bar marks a performance drop or a possible reset of
game settings.

## Ratings

- **building baseline** – three sessions with frame data are needed before
  anything is compared.
- **normal** – within the usual range of the game.
- **drop** – clearly slower than the median of the last good sessions.
- **better** – clearly faster.
- **no frame data** – session without measurement (PresentMon missing, no
  rights, or played too briefly).

### Reading the suspect list

Blackbox lists all changes between the last good session and the slow one,
most likely first. That is a hint, not proof. Only a counter-test brings
certainty: undo one change, play again, compare.

Besides system changes, **programs with background load** appear there:
during every session Blackbox measures which other programs use noticeable
CPU time (from 1 % of the whole CPU). A program that used at least 2 % in the
slow session and nothing or far less in the good ones becomes a suspect – a
virus scan or an update service, for example.

If the list is empty, the cause lies outside of what is recorded, such as
temperature or a more demanding game scene.

### Excluding untypical sessions

A session on another map, a benchmark or a short try distorts the
comparison. "Exclude" in the session table (or `blackbox exclude <no>`) takes
it out; "Include again" undoes that.

### Report for forums or support

"Copy report" on a drop puts a Markdown text on the clipboard: figures
compared with the baseline, suspects, background load and the most important
system data (Windows build, drivers, BIOS, power plan, gaming settings in
Windows). User names, paths, Steam account names and startup apps are not
included.

### Notifications

On a performance drop or a possible reset of game settings, Windows shows a
message even when Blackbox only runs in the notification area. Turn it off
under Settings › Notifications.

## Loadout Vault

Loadout Vault backs up the settings files of your games (graphics, key
bindings, sensitivity) whenever they change. Save games are not backed up.

- **Possible reset** – many settings changed at once, often right after a
  game update. "Differences" shows every changed setting with its old and new
  value.
- **Restore** – writes an earlier version back. The current version is
  backed up first, so a restore can be undone. The game must be closed.
- **Controls & gameplay only** – takes only the personal settings (keys,
  sensitivity, audio, gameplay options) from an earlier version. Resolution,
  refresh rate, window mode and graphics quality stay as they are. In the
  diff, the "Type" column shows what a setting counts as. Available for INI,
  Valve (`.vcfg`, `video.txt`, `.lst`) and `.cfg` files.
- Changes to settings also appear in the change journal and as suspects if
  performance drops afterwards.

Detected automatically: Unreal Engine games, Source 2 games (CS2, Dota 2,
Deadlock) as well as Elden Ring, Red Dead Redemption 2 and GTA V. For other
games you add the files yourself (see Configuration).

Note: if you used to make settings files read-only so patches cannot
overwrite them, you can remove that. Blackbox does not restore read-only
files.

### Moving to a new PC

1. Old PC: Loadout Vault › "Export profile", save the file.
2. New PC: install Blackbox, start each game once (so its settings files
   exist), then "Import profile".
3. The preview shows per file how many personal settings would be applied.
   Display and graphics of the new PC stay untouched.

If several Steam accounts are signed in on the target PC, a file is only
applied when the account is unambiguous; otherwise it is skipped.

## Command line

`blackbox.exe` (in the `cli` subfolder after installation) offers the same
functions without an interface.

| Command | Effect |
|---|---|
| `blackbox scan` | Capture the system and game settings once |
| `blackbox watch` | Keep running like the app, stop with Ctrl+C |
| `blackbox changes --days 30` | Change journal |
| `blackbox sessions [--game steam:730]` | Sessions with their rating |
| `blackbox analyze [--game steam:730]` | Drops with suspects |
| `blackbox games` | Detected games and their ids |
| `blackbox vault list` | Tracked games and files |
| `blackbox vault history --game steam:730` | Backed-up versions of a game |
| `blackbox vault diff <no>` | Differences of a version to the previous one |
| `blackbox vault restore <no> --yes` | Write a version back (without `--yes` preview only) |
| `blackbox vault restore <no> --personal --yes` | Apply personal settings only |
| `blackbox vault export profile.json [--game steam:730]` | Save a profile for another PC |
| `blackbox vault import profile.json [--yes]` | Apply a profile (without `--yes` preview only) |
| `blackbox exclude <no>` / `blackbox include <no>` | Exclude a session from comparison / include it again |
| `blackbox report <no> [--out report.md]` | Report of a session (Markdown) |
| `blackbox import --game steam:730 --csv file.csv --exe cs2.exe` | Import a PresentMon/CapFrameX capture as a session |
| `blackbox set-presentmon <path>` | Set PresentMon |
| `blackbox set-language auto\|de\|en` | Set the language |
| `blackbox --lang de <command>` | Print once in another language |
| `blackbox info` | Storage locations and configuration |

The app and the command line share the same data. Do not run
`blackbox watch` while the app is running, or every session is recorded
twice.

## Configuration

`config.json` (path: see `blackbox info` or the settings in the app):

| Key | Default | Meaning |
|---|---|---|
| `presentmon_path` | – | Own path to PresentMon.exe; empty = bundled PresentMon |
| `scan_interval_secs` | 900 | Time between two scans |
| `poll_interval_secs` | 5 | Interval of game detection |
| `min_frames` | 3000 | Sessions with fewer frames get no figures |
| `ignored_game_ids` | Wallpaper Engine, Soundpad | Library entries that do not open sessions |
| `vault_enabled` | `true` | Loadout Vault on/off |
| `vault_custom_paths` | – | Additional files or folders per game |
| `notifications` | `true` | Windows notifications for drops and resets |
| `language` | `auto` | Language: `auto` (Windows language), `de` or `en` |

Example for own paths (folders are backed up without subfolders):

```json
{
  "vault_custom_paths": {
    "steam:1086940": ["C:\\Users\\me\\AppData\\Local\\Larian Studios\\Baldur's Gate 3\\PlayerProfiles\\Public"]
  }
}
```

`blackbox games` shows the game id.

## Troubleshooting

- **"PresentMon stopped early"** – missing rights. Choose "Allow frame capture
  permanently" in the settings or start Blackbox as administrator.
- **A game is not detected** – does it appear in `blackbox games`? Steam,
  Epic, GOG, Battle.net, EA app, Ubisoft Connect and Riot are detected.
- **Restore refused** – the game is still running or the file is read-only.
- **More details** – logs under `%LOCALAPPDATA%\Blackbox\data\logs\`.
