//! Ready-to-display view of everything Blackbox knows: per game the
//! sessions with their verdicts and, for regressions, the ranked suspects;
//! plus a plain-text report for forums or support.

use serde::Serialize;

use crate::analysis::{analyze_sessions, make_suspect, rank_suspects, sort_suspects, AnalysisConfig, SessionAnalysis, Suspect, Verdict};
use crate::error::{Error, Result};
use crate::i18n;
use crate::load::{background_suspects, ProcessLoad};
use crate::model::{Category, ChangeEvent, ChangeKind, Session};
use crate::store::Store;

#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub session: Session,
    pub verdict: Verdict,
    /// Start of the last good session; only set for regressions.
    pub last_good_at: Option<i64>,
    /// Only filled for regressions.
    pub suspects: Vec<Suspect>,
    /// Programs that loaded the CPU during the session.
    pub load: Vec<ProcessLoad>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameReport {
    pub id: String,
    pub name: String,
    /// Oldest first.
    pub sessions: Vec<SessionReport>,
    /// Whether the most recent counted session with frame data is a regression.
    pub regressed: bool,
}

/// Suspects for a regressed session that followed the good session at `since`.
///
/// System changes count up to the session start. Config changes of the game
/// itself count until the session end: games rewrite or reset their settings
/// while starting, so such a change is only detected during the session.
/// Programs that loaded the CPU only in this session (compared with the
/// baseline sessions) are added as background-load suspects.
pub fn suspects_for(store: &Store, session: &Session, since: i64, baseline_ids: &[i64]) -> Result<Vec<Suspect>> {
    let until = session.ended_at.unwrap_or(session.started_at).max(session.started_at);
    let changes: Vec<_> = store
        .changes_between(since, until)?
        .into_iter()
        .filter(|c| {
            c.detected_at <= session.started_at
                || (c.category == Category::GameConfig && c.key == session.game_id)
        })
        .collect();
    let mut suspects = rank_suspects(&changes, &session.game_id);

    if session.load_samples.is_some() {
        let mut good = Vec::new();
        for id in baseline_ids {
            if let Some(s) = store.session(*id)? {
                if s.load_samples.is_some() {
                    good.push(store.session_load(s.id)?);
                }
            }
        }
        for (i, ls) in background_suspects(&store.session_load(session.id)?, &good).into_iter().enumerate() {
            let base = format!("{:.1}", ls.baseline_max);
            let before = if ls.baseline_max < 0.5 {
                crate::i18n::t("in guten Sessions nicht aktiv", "not active in good sessions").to_string()
            } else {
                crate::tr!("max. {base} % CPU in guten Sessions", "max. {base} % CPU in good sessions")
            };
            let (avg, peak) = (format!("{:.1}", ls.process.avg_cpu), format!("{:.0}", ls.process.peak_cpu));
            suspects.push(make_suspect(ChangeEvent {
                id: -(i as i64) - 1,
                detected_at: session.started_at,
                category: Category::BackgroundLoad,
                key: ls.process.name.clone(),
                label: ls.process.name.clone(),
                kind: ChangeKind::Modified,
                old_value: Some(before),
                new_value: Some(crate::tr!("Ø {avg} % CPU, Spitze {peak} %", "avg. {avg} % CPU, peak {peak} %")),
            }));
        }
        sort_suspects(&mut suspects);
    }
    Ok(suspects)
}

fn report_for(store: &Store, session: Session, a: SessionAnalysis) -> Result<SessionReport> {
    let suspects = match (&a.verdict, a.last_good_at) {
        (Verdict::Regression { .. }, Some(since)) => suspects_for(store, &session, since, &a.baseline_session_ids)?,
        _ => Vec::new(),
    };
    let load = if session.load_samples.is_some() { store.session_load(session.id)? } else { Vec::new() };
    Ok(SessionReport { session, verdict: a.verdict, last_good_at: a.last_good_at, suspects, load })
}

/// Report of one game, sessions oldest first.
pub fn game_report(store: &Store, game_id: &str, name: &str, cfg: &AnalysisConfig) -> Result<GameReport> {
    let sessions = store.sessions(Some(game_id))?;
    let analysis = analyze_sessions(&sessions, cfg);
    let mut reports = Vec::with_capacity(sessions.len());
    for (session, a) in sessions.into_iter().zip(analysis) {
        reports.push(report_for(store, session, a)?);
    }
    let regressed = reports
        .iter()
        .rev()
        .find(|r| r.session.metrics.is_some() && !r.session.excluded)
        .is_some_and(|r| matches!(r.verdict, Verdict::Regression { .. }));
    Ok(GameReport { id: game_id.to_string(), name: name.to_string(), sessions: reports, regressed })
}

/// Reports for all games that have at least one session, sorted by name.
pub fn game_reports(store: &Store, cfg: &AnalysisConfig) -> Result<Vec<GameReport>> {
    store
        .games_with_sessions()?
        .into_iter()
        .map(|(id, name)| game_report(store, &id, &name, cfg))
        .collect()
}

fn fmt_pct(v: f64) -> String {
    format!("{}{:.0} %", if v < 0.0 { "−" } else { "+" }, (v * 100.0).abs())
}

/// Labels may name a Steam account ("file · Name"); reports leave that out.
fn public_label(label: &str) -> &str {
    label.split(" · ").next().unwrap_or(label)
}

fn change_text(c: &ChangeEvent) -> String {
    let c = i18n::display_change(c);
    let label = public_label(&c.label);
    let detail = match (c.kind, &c.old_value, &c.new_value) {
        (ChangeKind::Added, _, Some(n)) => crate::tr!("neu: {n}", "new: {n}"),
        (ChangeKind::Removed, Some(o), _) => crate::tr!("entfernt (war: {o})", "removed (was: {o})"),
        (_, Some(o), Some(n)) => format!("{o} → {n}"),
        (_, None, Some(n)) => n.clone(),
        _ => String::new(),
    };
    format!("[{}] {label}: {detail}", c.category.label())
}

/// Markdown report of one session, ready to paste into a forum or a support
/// request, in the current language. Contains no user names, paths, Steam
/// account names or autostart entries.
pub fn session_report_text(store: &Store, session_id: i64, cfg: &AnalysisConfig) -> Result<String> {
    use crate::i18n::t;
    let session = store
        .session(session_id)?
        .ok_or_else(|| Error::Other(crate::tr!("Session #{session_id} existiert nicht", "Session #{session_id} does not exist")))?;
    let game = game_report(store, &session.game_id, &session.game_name, cfg)?;
    let r = game
        .sessions
        .into_iter()
        .find(|r| r.session.id == session_id)
        .expect("session belongs to its game");
    let mut out = String::new();
    let title = match r.verdict {
        Verdict::Regression { .. } => t("Leistungseinbruch", "Performance drop"),
        Verdict::Improvement { .. } => t("Leistung verbessert", "Performance improved"),
        _ => t("Session", "Session"),
    };
    let name = &r.session.game_name;
    out.push_str(&crate::tr!("## Blackbox-Bericht: {name} – {title}\n\n", "## Blackbox report: {name} – {title}\n\n"));
    let start = i18n::format_time(r.session.started_at);
    let duration = r.session.duration_secs().map(|d| format!(" ({} min)", d / 60)).unwrap_or_default();
    out.push_str(&crate::tr!("Session vom {start}{duration}\n\n", "Session on {start}{duration}\n\n"));

    match (&r.session.metrics, &r.verdict) {
        (Some(m), Verdict::Regression { baseline, avg_change, low_change } | Verdict::Improvement { baseline, avg_change, low_change }) => {
            let n = baseline.sessions;
            out.push_str(&crate::tr!(
                "| | Diese Session | Basis ({n} Sessions) | Änderung |\n|---|---|---|---|\n",
                "| | This session | Baseline ({n} sessions) | Change |\n|---|---|---|---|\n"
            ));
            let avg_label = t("Ø FPS", "Avg. FPS");
            out.push_str(&format!("| {avg_label} | {:.0} | {:.0} | {} |\n", m.avg_fps, baseline.avg_fps, fmt_pct(*avg_change)));
            out.push_str(&format!("| 1%-Low | {:.0} | {:.0} | {} |\n", m.p1_low_fps, baseline.p1_low_fps, fmt_pct(*low_change)));
            let stutter = t("Ruckler/min", "Stutters/min");
            out.push_str(&format!("| {stutter} | {:.1} | | |\n\n", m.stutters_per_min));
        }
        (Some(m), _) => {
            let (avg, low, st) = (format!("{:.0}", m.avg_fps), format!("{:.0}", m.p1_low_fps), format!("{:.1}", m.stutters_per_min));
            out.push_str(&crate::tr!(
                "Ø {avg} FPS, 1%-Low {low}, {st} Ruckler/min\n\n",
                "Avg. {avg} FPS, 1% low {low}, {st} stutters/min\n\n"
            ));
        }
        (None, _) => out.push_str(t("Keine Frame-Daten für diese Session.\n\n", "No frame data for this session.\n\n")),
    }

    if matches!(r.verdict, Verdict::Regression { .. }) {
        let since = r.last_good_at.map(i18n::format_time).unwrap_or_else(|| "–".into());
        if r.suspects.is_empty() {
            out.push_str(&crate::tr!(
                "Seit der letzten guten Session ({since}) wurde keine Änderung erfasst.\n\n",
                "No change was recorded since the last good session ({since}).\n\n"
            ));
        } else {
            out.push_str(&crate::tr!(
                "### Änderungen seit der letzten guten Session ({since})\n\n",
                "### Changes since the last good session ({since})\n\n"
            ));
            for (i, s) in r.suspects.iter().enumerate() {
                out.push_str(&format!("{}. {}\n", i + 1, change_text(&s.change)));
            }
            out.push('\n');
        }
    }

    if !r.load.is_empty() {
        out.push_str(t("### Hintergrundlast während der Session\n\n", "### Background load during the session\n\n"));
        for p in r.load.iter().take(5) {
            let (name, cpu) = (&p.name, format!("{:.1}", p.avg_cpu));
            out.push_str(&crate::tr!("- {name}: Ø {cpu} % CPU\n", "- {name}: avg. {cpu} % CPU\n"));
        }
        out.push('\n');
    }

    out.push_str(t("### System\n\n", "### System\n\n"));
    let shown = [Category::OsBuild, Category::GpuDriver, Category::Bios, Category::PowerPlan, Category::SystemSetting];
    let state = store.current_state()?;
    for cat in shown {
        for f in state.iter().filter(|f| f.category == cat).map(i18n::display_fact) {
            out.push_str(&format!("- {}: {}\n", f.label, f.value));
        }
    }
    let updates = state.iter().filter(|f| f.category == Category::WindowsUpdate).count();
    if updates > 0 {
        out.push_str(&crate::tr!("- Installierte Windows-Updates: {updates}\n", "- Installed Windows updates: {updates}\n"));
    }
    let version = env!("CARGO_PKG_VERSION");
    out.push_str(&crate::tr!(
        "\n_Erstellt mit Blackbox {version}. Zeigt zeitliche Zusammenhänge, keinen Beweis._\n",
        "\n_Created with Blackbox {version}. Shows correlations in time, not proof._\n"
    ));
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, Fact, SessionMetrics};

    fn play(store: &Store, start: i64, fps: f64) {
        let id = store.start_session("steam:1", "Game", start).unwrap();
        let m = SessionMetrics {
            frames: 10_000,
            avg_fps: fps,
            p1_low_fps: fps * 0.7,
            p01_low_fps: fps * 0.5,
            median_frametime_ms: 1000.0 / fps,
            stutters_per_min: 0.0,
        };
        store.finish_session(id, start + 600, Some(&m), None).unwrap();
    }

    fn load(name: &str, avg: f64) -> ProcessLoad {
        ProcessLoad { name: name.into(), avg_cpu: avg, peak_cpu: avg * 2.0 }
    }

    #[test]
    fn background_load_becomes_a_suspect_and_reaches_the_report() {
        let store = Store::open_in_memory().unwrap();
        for i in 1..=4 {
            play(&store, i * 1000, 100.0);
            store.save_session_load(i, 100, &[load("discord.exe", 3.0)]).unwrap();
        }
        play(&store, 5000, 60.0);
        store.save_session_load(5, 100, &[load("discord.exe", 3.2), load("MsMpEng.exe", 14.0)]).unwrap();
        // A config change of the game during the slow session, labelled with a Steam account.
        store
            .record_change(Category::GameConfig, "steam:1", "Game: video.txt · SecretName", crate::model::ChangeKind::Modified, None, Some("9 von 20 Einträgen geändert – möglicher Reset"), 5100)
            .unwrap();

        let reports = game_reports(&store, &AnalysisConfig::default()).unwrap();
        let bad = &reports[0].sessions[4];
        let cats: Vec<_> = bad.suspects.iter().map(|s| s.change.category).collect();
        assert_eq!(cats, vec![Category::GameConfig, Category::BackgroundLoad]);
        assert_eq!(bad.suspects[1].change.label, "MsMpEng.exe");
        assert_eq!(bad.suspects[1].change.old_value.as_deref(), Some("not active in good sessions"));
        assert_eq!(bad.load.len(), 2);

        let text = session_report_text(&store, 5, &AnalysisConfig::default()).unwrap();
        assert!(text.contains("Performance drop"));
        assert!(text.contains("[Background load] MsMpEng.exe"));
        assert!(text.contains("### Background load during the session"));
        assert!(!text.contains("SecretName"), "account names stay private:\n{text}");

        // Without load data in the good sessions there is nothing to compare.
        let other = Store::open_in_memory().unwrap();
        for i in 1..=4 {
            play(&other, i * 1000, 100.0);
        }
        play(&other, 5000, 60.0);
        other.save_session_load(5, 100, &[load("MsMpEng.exe", 14.0)]).unwrap();
        let r = game_reports(&other, &AnalysisConfig::default()).unwrap();
        assert!(r[0].sessions[4].suspects.iter().all(|s| s.change.category != Category::BackgroundLoad));
    }

    #[test]
    fn report_in_english() {
        let mut store = Store::open_in_memory().unwrap();
        let mode = Fact::new(Category::SystemSetting, "game_mode", "Spielmodus", "on");
        store.apply_scan(Category::SystemSetting, &[mode], 1).unwrap();
        for i in 1..=4 {
            play(&store, i * 1000, 100.0);
        }
        play(&store, 5000, 60.0);
        let text = i18n::with_lang(i18n::Lang::En, || session_report_text(&store, 5, &AnalysisConfig::default())).unwrap();
        assert!(text.starts_with("## Blackbox report: Game – Performance drop"), "{text}");
        assert!(text.contains("| Avg. FPS | 60 | 100 | −40 % |"), "{text}");
        assert!(text.contains("- Game Mode: on"), "system facts rendered in English: {text}");
        assert!(text.contains("Created with Blackbox"));
        assert!(!text.contains("Sitzung") && !text.contains("Änderung"), "{text}");
    }

    #[test]
    fn report_for_unknown_session_is_an_error() {
        let store = Store::open_in_memory().unwrap();
        assert!(session_report_text(&store, 42, &AnalysisConfig::default()).is_err());
    }

    #[test]
    fn regression_carries_suspects_and_marks_game() {
        let mut store = Store::open_in_memory().unwrap();
        let driver = |v: &str| vec![Fact::new(Category::GpuDriver, "gpu", "GPU", v)];
        store.apply_scan(Category::GpuDriver, &driver("1"), 0).unwrap();
        for i in 1..=4 {
            play(&store, i * 1000, 100.0);
        }
        store.apply_scan(Category::GpuDriver, &driver("2"), 4500).unwrap();
        play(&store, 5000, 60.0);

        let reports = game_reports(&store, &AnalysisConfig::default()).unwrap();
        assert_eq!(reports.len(), 1);
        let game = &reports[0];
        assert!(game.regressed);
        assert_eq!(game.sessions.len(), 5);
        assert!(game.sessions[..4].iter().all(|s| s.suspects.is_empty()));
        assert_eq!(game.sessions[4].suspects.len(), 1);
        assert_eq!(game.sessions[4].last_good_at, Some(4000));

        // A config reset detected during the bad session is a suspect too,
        // a system change during it is not.
        store
            .record_change(Category::GameConfig, "steam:1", "Game: a.ini", crate::model::ChangeKind::Modified, None, Some("reset"), 5100)
            .unwrap();
        store
            .record_change(Category::GameConfig, "steam:2", "Other: a.ini", crate::model::ChangeKind::Modified, None, Some("x"), 5100)
            .unwrap();
        store.apply_scan(Category::GpuDriver, &driver("3"), 5200).unwrap();
        let reports = game_reports(&store, &AnalysisConfig::default()).unwrap();
        let labels: Vec<_> = reports[0].sessions[4].suspects.iter().map(|s| s.change.label.as_str()).collect();
        assert_eq!(labels, vec!["GPU", "Game: a.ini"]);

        // A later healthy session clears the flag.
        play(&store, 6000, 100.0);
        let reports = game_reports(&store, &AnalysisConfig::default()).unwrap();
        assert!(!reports[0].regressed);
    }
}
