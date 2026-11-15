//! Blackbox desktop app: tray icon, background service and the commands the
//! web UI calls. All logic lives in `blackbox-core`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use blackbox_core::analysis::AnalysisConfig;
use blackbox_core::config::{Config, Paths};
use blackbox_core::model::{Category, ChangeEvent, Fact};
use blackbox_core::monitor::MonitorEvent;
use blackbox_core::report::{self, GameReport};
use blackbox_core::service::{Service, ServiceEvent};
use blackbox_core::i18n::{self, t};
use blackbox_core::store::Store;
use blackbox_core::tr;
use blackbox_core::system::{self, CaptureRights};
use blackbox_core::vault::{self, FileDiff, GameVault, ImportItem, KnownDirs, Profile, RestoreOutcome, Vault};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

/// How far back the change journal shown in the UI reaches.
const JOURNAL_DAYS: i64 = 180;
const UPDATED_EVENT: &str = "blackbox-updated";

#[derive(Debug, Clone, Default, Serialize)]
struct Status {
    /// Name of the game currently being recorded.
    active_game: Option<String>,
    active_game_id: Option<String>,
    /// Whether frames are captured for the active session.
    capturing: bool,
    last_scan_at: Option<i64>,
    /// Last error of the background service, if it is currently failing.
    error: Option<String>,
}

struct Shared {
    paths: Paths,
    status: Mutex<Status>,
    scan_requested: AtomicBool,
    reload_requested: AtomicBool,
    stop: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// Determined once: group membership only changes with a new sign-in.
    capture_rights: CaptureRights,
    /// Tray menu entries, relabelled when the language changes.
    tray_items: Mutex<Vec<MenuItem<tauri::Wry>>>,
}

#[derive(Serialize)]
struct Dashboard {
    games: Vec<GameReport>,
    /// Newest first.
    changes: Vec<ChangeEvent>,
    state: Vec<Fact>,
    vault: Vec<GameVault>,
    vault_enabled: bool,
    status: Status,
    /// Path set by the user, if any.
    presentmon_path: Option<String>,
    /// PresentMon shipped with the installer, if present.
    presentmon_bundled: Option<String>,
    /// Whether the configured PresentMon file exists.
    presentmon_ok: bool,
    category_labels: BTreeMap<&'static str, &'static str>,
    database: String,
    log_dir: String,
    autostart: bool,
    capture_rights: CaptureRights,
    notifications: bool,
    /// Language in use (`de`/`en`), the stored setting (`auto`/`de`/`en`) and
    /// the locale for dates and numbers.
    language: &'static str,
    language_setting: String,
    locale: &'static str,
}

fn build_dashboard(shared: &Shared) -> blackbox_core::Result<Dashboard> {
    let store = Store::open(&shared.paths.database)?;
    let config = Config::load(&shared.paths.config_file)?;
    let now = blackbox_core::now();
    let mut changes = store.changes_between(now - JOURNAL_DAYS * 86_400, now)?;
    changes.reverse();
    // Texts are rendered in the current language here, so the UI receives
    // ready-to-show labels for journal entries, facts and session notes.
    let mut games = report::game_reports(&store, &AnalysisConfig::default())?;
    for g in &mut games {
        for r in &mut g.sessions {
            r.session.capture_note = r.session.capture_note.as_deref().map(i18n::capture_note);
            for s in &mut r.suspects {
                s.change = i18n::display_change(&s.change);
            }
        }
    }
    let lang = i18n::lang();
    Ok(Dashboard {
        games,
        changes: changes.iter().map(i18n::display_change).collect(),
        state: store.current_state()?.iter().map(i18n::display_fact).collect(),
        vault: if config.vault_enabled { Vault::open(&shared.paths.vault_dir)?.overview()? } else { Vec::new() },
        vault_enabled: config.vault_enabled,
        status: shared.status.lock().unwrap().clone(),
        presentmon_ok: config.resolve_presentmon().is_some(),
        presentmon_bundled: blackbox_core::config::bundled_presentmon().map(|p| p.to_string_lossy().into_owned()),
        presentmon_path: config.presentmon_path.map(|p| p.to_string_lossy().into_owned()),
        category_labels: Category::ALL.iter().map(|c| (c.as_str(), c.label())).collect(),
        database: shared.paths.database.to_string_lossy().into_owned(),
        log_dir: shared.paths.log_dir.to_string_lossy().into_owned(),
        autostart: system::read_run_entry(system::RUN_KEY, system::RUN_VALUE).is_some(),
        capture_rights: shared.capture_rights,
        notifications: config.notifications,
        language: lang.code(),
        language_setting: config.language.clone(),
        locale: lang.locale(),
    })
}

fn tray_labels() -> [&'static str; 3] {
    [t("Blackbox öffnen", "Open Blackbox"), t("Jetzt scannen", "Scan now"), t("Beenden", "Quit")]
}

/// Store and apply the language (`auto`, `de` or `en`).
#[tauri::command]
fn set_language(app: AppHandle, shared: State<'_, Arc<Shared>>, language: String) -> Result<(), String> {
    let language = language.trim().to_ascii_lowercase();
    if !["auto", "de", "en"].contains(&language.as_str()) {
        return Err(tr!("Unbekannte Sprache `{language}`.", "Unknown language `{language}`."));
    }
    let mut config = Config::load(&shared.paths.config_file).map_err(|e| e.to_string())?;
    config.language = language;
    config.save(&shared.paths.config_file).map_err(|e| e.to_string())?;
    i18n::set_lang(config.lang());
    for (item, label) in shared.tray_items.lock().unwrap().iter().zip(tray_labels()) {
        let _ = item.set_text(label);
    }
    let _ = app.emit(UPDATED_EVENT, ());
    Ok(())
}

#[tauri::command]
fn dashboard(shared: State<'_, Arc<Shared>>) -> Result<Dashboard, String> {
    build_dashboard(&shared).map_err(|e| {
        tracing::error!(error = %e, "dashboard failed");
        e.to_string()
    })
}

#[tauri::command]
fn vault_diff(shared: State<'_, Arc<Shared>>, snapshot_id: i64) -> Result<FileDiff, String> {
    Vault::open(&shared.paths.vault_dir)
        .and_then(|v| v.diff_with_previous(snapshot_id))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn vault_restore(shared: State<'_, Arc<Shared>>, snapshot_id: i64, personal_only: bool) -> Result<RestoreOutcome, String> {
    let run = || -> blackbox_core::Result<RestoreOutcome> {
        let mut v = Vault::open(&shared.paths.vault_dir)?;
        let (game_id, game_name) = v.snapshot_owner(snapshot_id)?;
        if shared.status.lock().unwrap().active_game_id.as_deref() == Some(game_id.as_str()) {
            return Err(blackbox_core::Error::Other(tr!(
                "{game_name} läuft gerade. Bitte das Spiel zuerst beenden – es würde die Datei beim Beenden wieder überschreiben.",
                "{game_name} is running. Please close the game first – it would overwrite the file again when it exits."
            )));
        }
        let store = Store::open(&shared.paths.database)?;
        vault::restore_and_journal(&mut v, &store, snapshot_id, personal_only, blackbox_core::now())
    };
    run().map_err(|e| {
        tracing::warn!(error = %e, snapshot_id, "restore failed");
        e.to_string()
    })
}

/// Write the personal settings of all games to `path` (chosen by the user
/// in a save dialog). Returns the number of exported files.
#[tauri::command]
fn vault_export(shared: State<'_, Arc<Shared>>, path: String) -> Result<usize, String> {
    let run = || -> blackbox_core::Result<usize> {
        let profile = Vault::open(&shared.paths.vault_dir)?.export_profile(&[], blackbox_core::now())?;
        let files = profile.games.iter().map(|g| g.files.len()).sum();
        if files == 0 {
            return Err(blackbox_core::Error::Other(t("Keine exportierbaren Einstellungen gefunden.", "No exportable settings found.").into()));
        }
        let json = serde_json::to_string_pretty(&profile).map_err(|e| blackbox_core::Error::Other(e.to_string()))?;
        std::fs::write(&path, json)?;
        tracing::info!(files, "profile exported");
        Ok(files)
    };
    run().map_err(|e| e.to_string())
}

/// Preview (`apply == false`) or apply a profile file.
#[tauri::command(async)]
fn vault_import(shared: State<'_, Arc<Shared>>, path: String, apply: bool) -> Result<Vec<ImportItem>, String> {
    let run = || -> blackbox_core::Result<Vec<ImportItem>> {
        let text = std::fs::read_to_string(&path)?;
        let profile: Profile = serde_json::from_str(&text)
            .map_err(|_| blackbox_core::Error::Other(t("Die Datei ist kein gültiges Blackbox-Profil.", "The file is not a valid Blackbox profile.").into()))?;
        if let Some(active) = shared.status.lock().unwrap().active_game_id.clone() {
            if let Some(g) = profile.games.iter().find(|g| g.game_id == active) {
                let name = &g.game_name;
                return Err(blackbox_core::Error::Other(tr!("{name} läuft gerade. Bitte das Spiel zuerst beenden.", "{name} is running. Please close the game first.")));
            }
        }
        let config = Config::load(&shared.paths.config_file)?;
        let store = Store::open(&shared.paths.database)?;
        let mut v = Vault::open(&shared.paths.vault_dir)?;
        // The vault must know this PC's files and their current state.
        let games = blackbox_core::launchers::installed_games()?;
        vault::check_and_journal(&mut v, &store, &games, &KnownDirs::detect(), &config.vault_custom_paths, None, blackbox_core::now())?;
        if apply {
            vault::import_and_journal(&mut v, &store, &profile, blackbox_core::now())
        } else {
            v.import_profile(&profile, false, blackbox_core::now())
        }
    };
    run().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_session_excluded(shared: State<'_, Arc<Shared>>, session_id: i64, excluded: bool) -> Result<(), String> {
    let store = Store::open(&shared.paths.database).map_err(|e| e.to_string())?;
    match store.set_excluded(session_id, excluded) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("Session #{session_id} existiert nicht.")),
        Err(e) => Err(e.to_string()),
    }
}

/// Markdown report of a session for forums or support.
#[tauri::command]
fn session_report(shared: State<'_, Arc<Shared>>, session_id: i64) -> Result<String, String> {
    Store::open(&shared.paths.database)
        .and_then(|s| report::session_report_text(&s, session_id, &AnalysisConfig::default()))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_notifications(shared: State<'_, Arc<Shared>>, enabled: bool) -> Result<(), String> {
    let mut config = Config::load(&shared.paths.config_file).map_err(|e| e.to_string())?;
    config.notifications = enabled;
    config.save(&shared.paths.config_file).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let command = system::autostart_command(&exe);
    system::write_run_entry(system::RUN_KEY, system::RUN_VALUE, enabled.then_some(command.as_str()))
        .map_err(|e| e.to_string())?;
    tracing::info!(enabled, "autostart changed");
    Ok(())
}

/// Shows the Windows UAC prompt; runs off the main thread so the window
/// stays responsive while the prompt is open.
#[tauri::command(async)]
fn request_capture_rights() -> Result<(), String> {
    system::request_capture_rights().map_err(|e| e.to_string())
}

#[tauri::command]
fn scan_now(shared: State<'_, Arc<Shared>>) {
    shared.scan_requested.store(true, Ordering::SeqCst);
}

/// Store the path to PresentMon. An empty path disables frame capture.
#[tauri::command]
fn set_presentmon(shared: State<'_, Arc<Shared>>, path: String) -> Result<(), String> {
    let path = path.trim().trim_matches('"');
    let mut config = Config::load(&shared.paths.config_file).map_err(|e| e.to_string())?;
    if path.is_empty() {
        config.presentmon_path = None;
    } else {
        let path = PathBuf::from(path);
        // This file gets executed, so accept nothing that is not plausibly
        // PresentMon itself.
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !path.is_file() {
            return Err(t("Die Datei wurde nicht gefunden.", "The file was not found.").into());
        }
        if !(name.starts_with("presentmon") && name.ends_with(".exe")) {
            return Err(t("Erwartet wird die Datei PresentMon-…exe.", "Expected a file named PresentMon-…exe.").into());
        }
        config.presentmon_path = Some(path);
    }
    config.save(&shared.paths.config_file).map_err(|e| e.to_string())?;
    shared.reload_requested.store(true, Ordering::SeqCst);
    Ok(())
}

/// Show Windows notifications for regressions and settings resets.
fn send_notices(app: &AppHandle, shared: &Shared, events: &[ServiceEvent]) {
    use tauri_plugin_notification::NotificationExt;
    if events.is_empty() || !Config::load(&shared.paths.config_file).map(|c| c.notifications).unwrap_or(true) {
        return;
    }
    let notices = Store::open(&shared.paths.database).and_then(|s| blackbox_core::notify::notices_for(&s, events));
    match notices {
        Ok(list) => {
            for n in list {
                if let Err(e) = app.notification().builder().title(&n.title).body(&n.body).show() {
                    tracing::warn!(error = %e, "notification failed");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "notices could not be computed"),
    }
}

fn start_service(shared: &Shared) -> blackbox_core::Result<(Service, Duration)> {
    let config = Config::load(&shared.paths.config_file)?;
    let store = Store::open(&shared.paths.database)?;
    let service = Service::new(store, &config, &shared.paths)?;
    Ok((service, Duration::from_secs(config.poll_interval_secs)))
}

/// Background loop: owns the service, publishes status, notifies the UI.
fn worker(app: AppHandle, shared: Arc<Shared>) {
    let mut service: Option<(Service, Duration)> = None;
    while !shared.stop.load(Ordering::SeqCst) {
        let mut notify = false;

        // (Re)start the service; a broken config must not kill the app, so
        // failures are shown in the UI and retried.
        let idle = service.as_ref().is_none_or(|(s, _)| s.active_game().is_none());
        if service.is_none() || (idle && shared.reload_requested.swap(false, Ordering::SeqCst)) {
            match start_service(&shared) {
                Ok(s) => {
                    service = Some(s);
                    shared.status.lock().unwrap().error = None;
                }
                Err(e) => {
                    tracing::error!(error = %e, "service could not start");
                    shared.status.lock().unwrap().error = Some(e.to_string());
                    service = None;
                }
            }
            notify = true;
        }

        let mut pause = Duration::from_secs(5);
        if let Some((svc, poll_every)) = service.as_mut() {
            pause = *poll_every;
            if shared.scan_requested.swap(false, Ordering::SeqCst) {
                svc.request_scan();
                notify = true;
            }
            match svc.tick() {
                Ok(events) => {
                    send_notices(&app, &shared, &events);
                    let mut status = shared.status.lock().unwrap();
                    if status.error.take().is_some() {
                        notify = true;
                    }
                    for e in &events {
                        notify = true;
                        match e {
                            ServiceEvent::Changes(_) => {}
                            ServiceEvent::Session(MonitorEvent::SessionStarted {
                                game,
                                capturing,
                                ..
                            }) => {
                                status.active_game = Some(game.name.clone());
                                status.active_game_id = Some(game.id.clone());
                                status.capturing = *capturing;
                            }
                            ServiceEvent::Session(MonitorEvent::SessionEnded { .. }) => {
                                status.active_game = None;
                                status.active_game_id = None;
                                status.capturing = false;
                            }
                        }
                    }
                    if status.last_scan_at != svc.last_scan_at() {
                        status.last_scan_at = svc.last_scan_at();
                        notify = true;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "service tick failed");
                    shared.status.lock().unwrap().error = Some(e.to_string());
                    notify = true;
                }
            }
        }

        if notify {
            let _ = app.emit(UPDATED_EVENT, ());
        }
        let wake = Instant::now() + pause;
        while Instant::now() < wake
            && !shared.stop.load(Ordering::SeqCst)
            && !shared.scan_requested.load(Ordering::SeqCst)
            && !shared.reload_requested.load(Ordering::SeqCst)
        {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if let Some((mut svc, _)) = service {
        if let Err(e) = svc.shutdown() {
            tracing::error!(error = %e, "shutdown failed");
        }
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn init_logging(paths: &Paths) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    std::fs::create_dir_all(&paths.log_dir).ok()?;
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("blackbox-app")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&paths.log_dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        .init();
    Some(guard)
}

fn main() {
    let paths = Paths::default_locations().expect("user profile directory must exist");
    i18n::set_lang(Config::load(&paths.config_file).map(|c| c.lang()).unwrap_or_else(|_| i18n::system_lang()));
    let _log_guard = init_logging(&paths);

    let shared = Arc::new(Shared {
        paths,
        status: Mutex::new(Status::default()),
        scan_requested: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        worker: Mutex::new(None),
        capture_rights: system::capture_rights(),
        tray_items: Mutex::new(Vec::new()),
    });
    let start_hidden = std::env::args().any(|a| a == system::HIDDEN_ARG);

    let app = tauri::Builder::default()
        // Two instances would record every session twice.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            dashboard,
            scan_now,
            set_presentmon,
            vault_diff,
            vault_restore,
            vault_export,
            vault_import,
            set_autostart,
            request_capture_rights,
            set_session_excluded,
            session_report,
            set_notifications,
            set_language
        ])
        .setup({
            let shared = shared.clone();
            move |app| {
                let [open_label, scan_label, quit_label] = tray_labels();
                let open = MenuItem::with_id(app, "open", open_label, true, None::<&str>)?;
                let scan = MenuItem::with_id(app, "scan", scan_label, true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", quit_label, true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&open, &scan, &quit])?;
                *shared.tray_items.lock().unwrap() = vec![open.clone(), scan.clone(), quit.clone()];
                let mut tray = TrayIconBuilder::new()
                    .tooltip("Blackbox")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event({
                        let shared = shared.clone();
                        move |app, event| match event.id.as_ref() {
                            "open" => show_main_window(app),
                            "scan" => shared.scan_requested.store(true, Ordering::SeqCst),
                            "quit" => app.exit(0),
                            _ => {}
                        }
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_main_window(tray.app_handle());
                        }
                    });
                if let Some(icon) = app.default_window_icon() {
                    tray = tray.icon(icon.clone());
                }
                tray.build(app)?;

                // Keep an existing autostart entry pointing at this executable
                // (e.g. after an update installed to another folder).
                if let (Some(current), Ok(exe)) =
                    (system::read_run_entry(system::RUN_KEY, system::RUN_VALUE), std::env::current_exe())
                {
                    let wanted = system::autostart_command(&exe);
                    if current != wanted {
                        if let Err(e) = system::write_run_entry(system::RUN_KEY, system::RUN_VALUE, Some(&wanted)) {
                            tracing::warn!(error = %e, "autostart entry not updated");
                        }
                    }
                }
                if !start_hidden {
                    show_main_window(app.handle());
                }

                let handle = app.handle().clone();
                let worker_shared = shared.clone();
                let join = std::thread::Builder::new()
                    .name("blackbox-service".into())
                    .spawn(move || worker(handle, worker_shared))?;
                *shared.worker.lock().unwrap() = Some(join);
                Ok(())
            }
        })
        .on_window_event(|window, event| {
            // Closing the window keeps Blackbox recording in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("Blackbox could not start");

    app.run(move |_app, event| {
        if let RunEvent::Exit = event {
            // Let the worker close a running session before the process ends.
            shared.stop.store(true, Ordering::SeqCst);
            if let Some(join) = shared.worker.lock().unwrap().take() {
                let _ = join.join();
            }
        }
    });
}
