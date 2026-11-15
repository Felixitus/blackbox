# Architecture

## Overview

```
crates/core      blackbox-core   library: all logic, no interface
crates/cli       blackbox        command line tool
app/src-tauri    blackbox-app    desktop app (Tauri 2): notification area, background service
app/ui                           interface (HTML/CSS/JS without a build step)
```

All logic lives in `blackbox-core`. The CLI and the desktop app are thin
layers on top and share the background service (`service::Service`), so both
behave the same.

## Modules in `blackbox-core`

| Module | Purpose |
|---|---|
| `model` | Data types: `Fact`, `ChangeEvent`, `Game`, `Session`, `SessionMetrics` |
| `collectors` | Each reads one aspect of the system and returns `Fact`s |
| `steam`, `vdf` | Read Steam libraries, game manifests, account names |
| `launchers` | Games of all launchers: Steam, Epic (manifests), GOG (registry), Battle.net/EA/Ubisoft/Riot (uninstall list) |
| `system` | Autostart entry, checking and setting up the rights for PresentMon |
| `store` | SQLite: current state, change journal, sessions |
| `monitor` | Detects running games, starts and ends sessions, drives PresentMon |
| `frames` | Read the PresentMon CSV, compute figures |
| `analysis` | Detect drops, rank suspects |
| `report` | Ready-to-show view per game: sessions, ratings, suspects |
| `service` | Background loop: scans, vault checks, sessions |
| `load` | Collect background load per session and compare it with good sessions |
| `notify` | Decides which events trigger a Windows notification |
| `i18n` | English/German: language choice, `tr!` macro, rendering of stored codes |
| `vault` | Loadout Vault (see below) |
| `config` | Configuration file and storage locations |

## Data flow

1. **Scan.** Every collector returns the facts of its category.
   `Store::apply_scan` compares them with the stored state and writes one
   `ChangeEvent` per difference. The first scan of a category only
   establishes the starting point.
2. **Vault check.** Right after every scan, and 90 seconds after a game
   starts (for the running game only), the settings files are checked. Every
   change is backed up and written to the journal as a `GameConfig` event.
3. **Session.** `Monitor::poll` checks the process list. If a process runs
   from the install folder of a known game, a session begins and PresentMon
   is started. When the game ends, the CSV is condensed into figures and
   deleted.
4. **Analysis.** `analyze_sessions` compares every session with the median
   of up to five earlier good sessions of the same game. For a drop,
   `report::suspects_for` returns the changes since the last good session.

## Loadout Vault (`vault`)

| Part | Purpose |
|---|---|
| `discover` | Finds the settings files of installed games |
| `parse` | Breaks INI, Valve KeyValues, Source `.cfg`, JSON and XML down into setting → value |
| `store` | SQLite index plus content-addressed copies (SHA-256), diff, restore, profile export/import |
| `selective` | Classifies every setting (display, graphics, personal), line-exact merging |

**Detection without a game database.** Three general rules cover many games:

- *Unreal Engine:* if `<install>/<Project>/Binaries` exists, the settings are
  under `%LOCALAPPDATA%/<Project>/Saved/Config/Windows*/`. Only
  `GameUserSettings.ini`, `Input.ini`, `Engine.ini`, `Game.ini` and
  `Scalability.ini` are backed up.
- *Source 2:* `Steam/userdata/<account>/<appid>/local/cfg` and key bindings
  from `.../remote/cfg`. With several Steam accounts, every file is labelled
  with the account name.
- *A fixed table* for known exceptions (Elden Ring, RDR2, GTA V).

Own files or folders can be added per game in `vault_custom_paths`.

**Reset detection.** A change counts as a possible reset if at least 5
settings and at least 30 % of all settings differ at once, or if the game was
updated since the last backup and at least 3 settings differ. Pure formatting
changes (same settings) are backed up silently.

**Restore.** Before writing back, the current version is backed up if it is
not yet. Writing goes through a temporary file followed by a rename. If the
game is running, the restore is refused, because the game would overwrite the
file again when it exits. Read-only files are not touched.

**Personal vs. hardware-dependent.** Every setting is classified by its
name: *display* (resolution, refresh rate, window mode, monitor), *graphics*
(quality levels, upscaling, anti-aliasing, FPS limit, prefixes such as `r.`,
`sg.`, `mat_`) or *personal* (everything else, above all keys, sensitivity,
audio, crosshair). A partial restore and a profile import write only personal
values into the target file, line by line; comments, order, line endings and
encoding (including UTF-16) of the target file are preserved. New keys are
not added.

**Profiles.** A profile is JSON with the text of the supported files per
game. On import, every file is matched to the file of the same name of the
same game: first of the same Steam account, otherwise only if there is
exactly one candidate. Ambiguous cases are skipped, so settings of one
account never end up in the files of another.

**Valve dialects.** Steam escapes backslashes (`"D:\\Lib"`), many games
write them literally (`"Key" "\"`). The parser tries both readings.

**Retention.** Up to 50 versions per file; older ones are deleted together
with copies that are no longer needed. Files over 2 MB are not backed up.

## Design decisions

- **No injection.** Game detection through the process list, frame capture
  through PresentMon (ETW). That keeps Blackbox compatible with anti-cheat.
- **Fault-tolerant collectors.** If a collector fails, its category is
  skipped for that scan. A read error never shows up as "everything was
  removed".
- **Robust thresholds.** A drop must be at least 10 % (average FPS) or 15 %
  (1 % low) *and* exceed three times the usual spread of the game.
- **Drops do not become the new normal.** Sessions flagged as drops do not
  enter the baseline.
- **Suspect window.** System changes count up to the session start. Changes
  to the settings of the affected game count until the session end, because
  games reset their settings while starting.
- **Raw data is discarded.** Only figures are stored; frame captures can be
  hundreds of MB.

## Languages

The language is process-wide (`i18n::set_lang`); tests set it with
`i18n::with_lang` for their own thread only and therefore run in parallel in
both languages. Texts sit in the code as a pair: `t("Deutsch", "English")`
or `tr!("{n} Dateien", "{n} files")`.

**Storage is language-neutral.** What ends up in the database are codes that
are only translated when shown: journal entries of the vault
(`vault:changed;n=9;total=20;reset=1;…`), session notes
(`note:too_few;n=12;min=3000`), Windows settings (`on`, `off`, `default`) and
file dates (`files 2026-…`). Switching the language therefore changes old
entries too and never creates spurious changes. When a database of an earlier
version is opened, its German values are converted to codes once; old free
texts in the journal are shown as they are.

The app renders the journal, system state, suspects and notes in the backend
(`i18n::display_change`, `display_fact`, `capture_note`); the interface only
translates its own texts. The CLI determines the language before parsing the
arguments (`--lang`, configuration, Windows language), so `--help` is
bilingual too.

## Background load

On every process poll during a session (every 5 seconds), the CPU load of
all other programs is recorded as a share of the whole CPU; processes of the
same name (browser tabs) are summed. At most 12 programs with an average of
at least 1 % are stored. The game itself, Blackbox, PresentMon and the
Windows idle process never count.

A program is a suspect if it used at least 2 % in the slow session and was
absent in the baseline sessions (below 0.5 %) or used at most a third of that
there. Only baseline sessions that have load data themselves are compared.

`sysinfo` computes the load of a process only from the second poll at least
200 ms apart and scales it by the number of known CPUs; that is why the
monitor reads the CPU list once at start.

## Tests

| Level | What |
|---|---|
| Unit tests | every module, including parsers, thresholds, merging, migration of old databases |
| `crates/core/tests/pipeline.rs` | scan → journal → drop → suspects |
| `crates/core/tests/capture.rs` | game detected → PresentMon stand-in (`examples/fake_presentmon.rs`, CSV in the 2.6 layout) → figures; a real CPU-heavy background program is measured; PresentMon without rights; session too short |
| `crates/cli/tests/cli.rs` | the real `blackbox` program with a test data directory: import, analysis, exclusion, report, exit codes, both languages |

## Limits

- Blackbox shows correlations in time, not proof.
- Sessions are noisy without a fixed test scene. The spread threshold dampens
  that but does not remove it.
- Reset detection is a heuristic: it cannot know whether new values are
  defaults, only that a lot changed at once.
- The classification into display, graphics and personal relies on name
  patterns and can be wrong for unusually named settings.
- Not recorded at present: temperatures, GPU load of other programs, settings
  in the registry (Unity games) or on the vendors' servers.

## Windows integration

- **Autostart:** value `Blackbox` under `HKCU\...\CurrentVersion\Run`,
  started with `--hidden` (notification area only). If the entry points to
  another executable (e.g. after a reinstall), the app corrects it on start.
- **Rights for PresentMon:** the app checks with `whoami /groups`, by SID
  (language-independent), whether it runs elevated or the account is in
  "Performance Log Users" (S-1-5-32-559). Setting it up starts, after a
  Windows confirmation, a PowerShell script with `Add-LocalGroupMember`; the
  user name is escaped and the script is passed Base64-encoded.
- **Installer:** Tauri NSIS, per-user install. The command line tool lives in
  the `cli` subfolder so it does not collide with the debug CLI of the same
  name in the build folder; the bundled PresentMon is looked for next to the
  executable or one level up. A custom hook deletes `%APPDATA%\Blackbox` and
  `%LOCALAPPDATA%\Blackbox` on uninstall only if "Delete app data" is ticked
  and it is not an update.

## Security and privacy

- No network access, no telemetry.
- Database, backups and configuration live in the user profile.
- PresentMon is bundled: pinned version 2.6.0, checked at build time against
  a fixed SHA-256 checksum (signed by Intel). The app itself downloads
  nothing. It is started without a shell and with fixed arguments, either
  the own path from the settings (only files named `PresentMon*.exe`) or the
  copy next to the executable.
- SQL only through bound parameters.
- The interface sets content only through `textContent` (no `innerHTML`); a
  Content Security Policy allows only its own scripts.
- Invisible control characters (bidi overrides, zero-width characters) are
  removed from game and account names.
