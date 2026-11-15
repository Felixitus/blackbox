//! Which events deserve a Windows notification, and what it says.
//!
//! Blackbox mostly runs unseen in the tray, so a regression or a settings
//! reset would go unnoticed without a message. Only those two events are
//! announced; everything else stays in the app.

use crate::analysis::{AnalysisConfig, Verdict};
use crate::error::Result;
use crate::i18n;
use crate::model::{Category, ChangeEvent};
use crate::monitor::MonitorEvent;
use crate::report;
use crate::service::ServiceEvent;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

/// Labels may name a Steam account ("file · Name"); notifications leave that out.
fn public(label: &str) -> &str {
    label.split(" · ").next().unwrap_or(label)
}

fn reset_notice(c: &ChangeEvent) -> Option<Notice> {
    let raw = c.new_value.as_deref()?;
    if c.category != Category::GameConfig || !i18n::is_reset_value(raw) {
        return None;
    }
    let shown = i18n::display_change(c);
    let summary = shown.new_value.unwrap_or_default();
    let label = public(&c.label);
    let (game, file) = label.split_once(": ").unwrap_or((label, ""));
    Some(Notice {
        title: crate::tr!("Möglicher Reset: {game}", "Possible reset: {game}"),
        body: crate::tr!(
            "{file} – {summary}. In Blackbox unter Loadout Vault prüfen und bei Bedarf wiederherstellen.",
            "{file} – {summary}. Check it in Blackbox under Loadout Vault and restore it if needed."
        ),
    })
}

fn regression_notice(store: &Store, session_id: i64, game_id: &str, game_name: &str) -> Result<Option<Notice>> {
    let game = report::game_report(store, game_id, game_name, &AnalysisConfig::default())?;
    let Some(r) = game.sessions.iter().find(|r| r.session.id == session_id) else { return Ok(None) };
    let Verdict::Regression { avg_change, low_change, .. } = r.verdict else { return Ok(None) };
    let (metric, change) = if avg_change <= low_change {
        (i18n::t("Ø FPS", "Avg. FPS"), avg_change)
    } else {
        (i18n::t("1%-Low", "1% low"), low_change)
    };
    let pct = format!("{:.0} %", change * 100.0).replace('-', "−");
    let cause = match r.suspects.first() {
        Some(s) => {
            let (cat, label) = (s.change.category.label(), public(&s.change.label).to_string());
            crate::tr!("Wahrscheinlichste Ursache: {cat} – {label}.", "Most likely cause: {cat} – {label}.")
        }
        None => i18n::t(
            "Seit der letzten guten Session wurde keine Änderung erfasst.",
            "No change was recorded since the last good session.",
        )
        .to_string(),
    };
    Ok(Some(Notice {
        title: crate::tr!("Leistungseinbruch in {game_name}", "Performance drop in {game_name}"),
        body: crate::tr!(
            "{metric} {pct} gegenüber den letzten Sessions. {cause}",
            "{metric} {pct} compared with recent sessions. {cause}"
        ),
    }))
}
/// Notifications for one batch of service events.
pub fn notices_for(store: &Store, events: &[ServiceEvent]) -> Result<Vec<Notice>> {
    let mut out: Vec<Notice> = Vec::new();
    for e in events {
        match e {
            ServiceEvent::Changes(changes) => {
                for n in changes.iter().filter_map(reset_notice) {
                    // One message per game, even if several of its files changed.
                    if !out.iter().any(|o| o.title == n.title) {
                        out.push(n);
                    }
                }
            }
            ServiceEvent::Session(MonitorEvent::SessionEnded { session_id, game, metrics: Some(_) }) => {
                if let Some(n) = regression_notice(store, *session_id, &game.id, &game.name)? {
                    out.push(n);
                }
            }
            ServiceEvent::Session(_) => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChangeKind, Fact, Game, SessionMetrics};

    fn game() -> Game {
        Game { id: "steam:1".into(), name: "Testspiel".into(), install_dir: "C:\\x".into(), build_id: "1".into() }
    }

    fn play(store: &Store, start: i64, fps: f64) -> (i64, SessionMetrics) {
        let id = store.start_session("steam:1", "Testspiel", start).unwrap();
        let m = SessionMetrics {
            frames: 10_000,
            avg_fps: fps,
            p1_low_fps: fps * 0.7,
            p01_low_fps: fps * 0.5,
            median_frametime_ms: 1000.0 / fps,
            stutters_per_min: 0.0,
        };
        store.finish_session(id, start + 600, Some(&m), None).unwrap();
        (id, m)
    }

    fn ended(id: i64, m: SessionMetrics) -> ServiceEvent {
        ServiceEvent::Session(MonitorEvent::SessionEnded { session_id: id, game: game(), metrics: Some(m) })
    }

    #[test]
    fn regression_is_announced_with_top_suspect() {
        let mut store = Store::open_in_memory().unwrap();
        store.apply_scan(Category::GpuDriver, &[Fact::new(Category::GpuDriver, "gpu", "RX 7900", "1")], 0).unwrap();
        for i in 1..=4 {
            let (id, m) = play(&store, i * 1000, 100.0);
            // Normal sessions are not announced.
            assert!(notices_for(&store, &[ended(id, m)]).unwrap().is_empty());
        }
        store.apply_scan(Category::GpuDriver, &[Fact::new(Category::GpuDriver, "gpu", "RX 7900", "2")], 4800).unwrap();
        let (id, m) = play(&store, 5000, 60.0);
        let n = notices_for(&store, &[ended(id, m)]).unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "Performance drop in Testspiel");
        assert!(n[0].body.starts_with("Avg. FPS −40 %"), "{}", n[0].body);
        assert!(n[0].body.contains("Graphics driver – RX 7900"));
    }

    #[test]
    fn notices_in_english() {
        let store = Store::open_in_memory().unwrap();
        let c = store
            .record_change(Category::GameConfig, "steam:730", "Counter-Strike 2: cs2_video.txt · Secret", ChangeKind::Modified, None, Some(&i18n::vault_changed_code(9, 20, true, true, false)), 10)
            .unwrap();
        let n = i18n::with_lang(i18n::Lang::En, || notices_for(&store, &[ServiceEvent::Changes(vec![c])])).unwrap();
        assert_eq!(n[0].title, "Possible reset: Counter-Strike 2");
        assert_eq!(
            n[0].body,
            "cs2_video.txt – 9 of 20 settings changed – possible reset after a game update. Check it in Blackbox under Loadout Vault and restore it if needed."
        );
    }

    #[test]
    fn sessions_without_metrics_are_silent() {
        let store = Store::open_in_memory().unwrap();
        let e = ServiceEvent::Session(MonitorEvent::SessionEnded { session_id: 1, game: game(), metrics: None });
        assert!(notices_for(&store, &[e]).unwrap().is_empty());
    }

    #[test]
    fn resets_are_announced_once_per_game_without_account_names() {
        let store = Store::open_in_memory().unwrap();
        let change = |label: &str, value: &str| {
            store.record_change(Category::GameConfig, "steam:730", label, ChangeKind::Modified, None, Some(value), 10).unwrap()
        };
        let events = [ServiceEvent::Changes(vec![
            change("Counter-Strike 2: cs2_video.txt · Geheim", "9 von 20 Einträgen geändert – möglicher Reset nach Spiel-Update"),
            change("Counter-Strike 2: cs2_user_keys.vcfg · Geheim", "12 von 30 Einträgen geändert – möglicher Reset"),
            change("Counter-Strike 2: cs2_user_convars.vcfg", "1 von 30 Einträgen geändert"),
        ])];
        let n = notices_for(&store, &events).unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "Possible reset: Counter-Strike 2");
        assert!(n[0].body.starts_with("cs2_video.txt – 9 von 20"));
        assert!(!n[0].body.contains("Geheim"));
    }
}
