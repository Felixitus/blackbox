//! Regression detection and suspect ranking.
//!
//! A session is compared with the median of the preceding sessions of the
//! same game. If it is clearly slower, every system change journaled since
//! the last good session becomes a *suspect*. Blackbox reports correlation,
//! not proof: the ranking says where to look first.

use serde::Serialize;

use crate::model::{Category, ChangeEvent, Session, SessionMetrics, Timestamp};

#[derive(Debug, Clone, Copy)]
pub struct AnalysisConfig {
    /// How many preceding sessions form the baseline.
    pub baseline_sessions: usize,
    /// Minimum number of baseline sessions required for a verdict.
    pub min_baseline_sessions: usize,
    /// Minimum relative drop of average FPS that counts as a regression.
    pub min_avg_drop: f64,
    /// Minimum relative drop of the 1 % low that counts as a regression.
    pub min_low_drop: f64,
    /// A drop must also exceed this many median absolute deviations of the
    /// baseline, so that naturally noisy games are not flagged constantly.
    pub noise_factor: f64,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        AnalysisConfig {
            baseline_sessions: 5,
            min_baseline_sessions: 3,
            min_avg_drop: 0.10,
            min_low_drop: 0.15,
            noise_factor: 3.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Baseline {
    pub sessions: usize,
    pub avg_fps: f64,
    pub p1_low_fps: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Verdict {
    /// The session itself has no frame data.
    NoData,
    /// Marked by the user as not representative.
    Excluded,
    /// Not enough earlier sessions with frame data.
    InsufficientHistory { have: usize, need: usize },
    Normal { baseline: Baseline },
    Regression {
        baseline: Baseline,
        /// Relative change, e.g. -0.25 = 25 % slower.
        avg_change: f64,
        low_change: f64,
    },
    Improvement {
        baseline: Baseline,
        avg_change: f64,
        low_change: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionAnalysis {
    pub session_id: i64,
    pub verdict: Verdict,
    /// Start of the last session that was still fine; changes after it are
    /// suspects. `None` unless the verdict is a regression.
    pub last_good_at: Option<Timestamp>,
    /// Sessions the baseline was computed from (empty without a baseline).
    pub baseline_session_ids: Vec<i64>,
}
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Median and median absolute deviation.
fn median_mad(values: &[f64]) -> (f64, f64) {
    let mut v = values.to_vec();
    let med = median(&mut v);
    let mut dev: Vec<f64> = values.iter().map(|x| (x - med).abs()).collect();
    (med, median(&mut dev))
}

/// Relative threshold a metric must fall below the baseline median by.
fn threshold(min_drop: f64, med: f64, mad: f64, noise_factor: f64) -> f64 {
    if med <= 0.0 {
        return min_drop;
    }
    min_drop.max(noise_factor * mad / med)
}

/// Analyse every session of one game. `sessions` must all belong to the same
/// game and be ordered oldest first (as returned by `Store::sessions`).
pub fn analyze_sessions(sessions: &[Session], cfg: &AnalysisConfig) -> Vec<SessionAnalysis> {
    struct Past {
        id: i64,
        started_at: Timestamp,
        metrics: SessionMetrics,
        good: bool,
    }
    let plain = |id: i64, verdict: Verdict| SessionAnalysis {
        session_id: id,
        verdict,
        last_good_at: None,
        baseline_session_ids: Vec::new(),
    };
    let mut out = Vec::with_capacity(sessions.len());
    let mut history: Vec<Past> = Vec::new();

    for s in sessions {
        if s.excluded {
            out.push(plain(s.id, Verdict::Excluded));
            continue;
        }
        let Some(m) = s.metrics else {
            out.push(plain(s.id, Verdict::NoData));
            continue;
        };

        // Regressed sessions are kept out of the baseline; otherwise a
        // persistent regression would quietly become the new normal after a
        // few sessions and the cause would drop out of the suspect window.
        let good: Vec<&Past> = history.iter().filter(|h| h.good).collect();
        let base: Vec<&Past> = good.iter().rev().take(cfg.baseline_sessions).copied().collect();

        if base.len() < cfg.min_baseline_sessions {
            out.push(plain(s.id, Verdict::InsufficientHistory { have: base.len(), need: cfg.min_baseline_sessions }));
            history.push(Past { id: s.id, started_at: s.started_at, metrics: m, good: true });
            continue;
        }

        let avgs: Vec<f64> = base.iter().map(|h| h.metrics.avg_fps).collect();
        let lows: Vec<f64> = base.iter().map(|h| h.metrics.p1_low_fps).collect();
        let (avg_med, avg_mad) = median_mad(&avgs);
        let (low_med, low_mad) = median_mad(&lows);
        let baseline = Baseline { sessions: base.len(), avg_fps: avg_med, p1_low_fps: low_med };

        let avg_change = if avg_med > 0.0 { m.avg_fps / avg_med - 1.0 } else { 0.0 };
        let low_change = if low_med > 0.0 { m.p1_low_fps / low_med - 1.0 } else { 0.0 };
        let avg_thr = threshold(cfg.min_avg_drop, avg_med, avg_mad, cfg.noise_factor);
        let low_thr = threshold(cfg.min_low_drop, low_med, low_mad, cfg.noise_factor);

        let regressed = avg_change <= -avg_thr || low_change <= -low_thr;
        let improved = !regressed && (avg_change >= avg_thr || low_change >= low_thr);

        let (verdict, last_good_at) = if regressed {
            (Verdict::Regression { baseline, avg_change, low_change }, good.last().map(|h| h.started_at))
        } else if improved {
            (Verdict::Improvement { baseline, avg_change, low_change }, None)
        } else {
            (Verdict::Normal { baseline }, None)
        };
        out.push(SessionAnalysis {
            session_id: s.id,
            verdict,
            last_good_at,
            baseline_session_ids: base.iter().map(|h| h.id).collect(),
        });
        history.push(Past { id: s.id, started_at: s.started_at, metrics: m, good: !regressed });
    }
    out
}
/// A change that may explain a regression.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suspect {
    pub change: ChangeEvent,
    /// 0..=1, higher = look here first.
    pub score: f64,
    /// German hint how to verify or undo the change.
    pub hint: &'static str,
}

fn category_weight(c: Category) -> f64 {
    match c {
        Category::GameBuild => 0.9,
        Category::GpuDriver => 0.9,
        Category::GameConfig => 0.8,
        Category::WindowsUpdate => 0.7,
        Category::OsBuild => 0.7,
        Category::SystemSetting => 0.6,
        Category::PowerPlan => 0.6,
        Category::Bios => 0.5,
        Category::Autostart => 0.3,
        Category::AudioDriver => 0.2,
        Category::BackgroundLoad => 0.75,
    }
}

fn hint(c: Category) -> &'static str {
    use crate::i18n::t;
    match c {
        Category::GameBuild => t(
            "Das Spiel wurde aktualisiert. Patchnotes und Foren auf Performance-Meldungen prüfen; bei Steam ggf. über „Betas“ auf den vorherigen Stand wechseln.",
            "The game was updated. Check the patch notes and forums for performance reports; on Steam you may switch back to the previous version under \"Betas\".",
        ),
        Category::GpuDriver => t(
            "Grafiktreiber wurde geändert. Zum Test die vorherige Version installieren (saubere Neuinstallation).",
            "The graphics driver changed. To test, install the previous version (clean install).",
        ),
        Category::GameConfig => t(
            "Die Spieleinstellungen wurden geändert oder zurückgesetzt. Vorherigen Stand vergleichen und wiederherstellen.",
            "The game settings were changed or reset. Compare with the previous version and restore it.",
        ),
        Category::WindowsUpdate => t(
            "Windows-Update wurde installiert oder entfernt. Unter Einstellungen › Windows Update › Updateverlauf lässt es sich testweise deinstallieren.",
            "A Windows update was installed or removed. To test, uninstall it under Settings › Windows Update › Update history.",
        ),
        Category::OsBuild => t(
            "Der Windows-Build hat sich geändert (kumulatives Update). Bekannte Probleme zum Update prüfen, ggf. Treiber-Hotfix des GPU-Herstellers installieren.",
            "The Windows build changed (cumulative update). Check known issues of the update; install a hotfix driver from the GPU vendor if available.",
        ),
        Category::SystemSetting => t(
            "Eine spielrelevante Windows-Einstellung wurde geändert. Zum Test auf den alten Wert zurückstellen.",
            "A gaming-related Windows setting changed. To test, set it back to the old value.",
        ),
        Category::PowerPlan => t(
            "Der Energieplan wurde gewechselt. Sparsame Pläne drosseln CPU und GPU; alten Plan wieder aktivieren.",
            "The power plan changed. Power-saving plans throttle CPU and GPU; switch back to the old plan.",
        ),
        Category::Bios => t(
            "BIOS/UEFI wurde aktualisiert. Dabei werden oft RAM-Profil (XMP/EXPO) und Lüfterkurven zurückgesetzt – im BIOS prüfen.",
            "The BIOS/UEFI was updated. This often resets the RAM profile (XMP/EXPO) and fan curves – check in the BIOS.",
        ),
        Category::Autostart => t(
            "Ein Autostart-Programm kam hinzu oder wurde geändert. Zum Test beenden bzw. im Task-Manager unter „Autostart“ deaktivieren.",
            "A startup app was added or changed. To test, close it or disable it in Task Manager under \"Startup apps\".",
        ),
        Category::AudioDriver => t(
            "Audiotreiber wurde geändert. Selten Ursache für FPS-Verlust, kann aber Ruckler durch hohe DPC-Latenz auslösen.",
            "The audio driver changed. Rarely a cause of lower FPS, but it can cause stutter through high DPC latency.",
        ),
        Category::BackgroundLoad => t(
            "Dieses Programm belegte während der langsamen Session spürbar CPU, in den guten Sessions nicht. Zum Test vor dem Spielen beenden oder seine Hintergrundaktivität abschalten.",
            "This program used noticeable CPU time during the slow session but not in the good ones. To test, close it before playing or turn off its background activity.",
        ),
    }
}
/// Turn a change into a suspect with its category's weight and hint.
pub fn make_suspect(change: ChangeEvent) -> Suspect {
    Suspect { score: category_weight(change.category), hint: hint(change.category), change }
}

/// Highest score first; among equals the most recent change first.
pub fn sort_suspects(suspects: &mut [Suspect]) {
    suspects.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.change.detected_at.cmp(&a.change.detected_at))
            .then(a.change.id.cmp(&b.change.id))
    });
}

/// Rank the changes that happened between the last good session and the
/// regressed one. Build changes of *other* games are irrelevant and dropped.
pub fn rank_suspects(changes: &[ChangeEvent], game_id: &str) -> Vec<Suspect> {
    let mut suspects: Vec<Suspect> = changes
        .iter()
        .filter(|c| {
            !matches!(c.category, Category::GameBuild | Category::GameConfig) || c.key == game_id
        })
        .cloned()
        .map(make_suspect)
        .collect();
    sort_suspects(&mut suspects);
    suspects
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChangeKind;

    fn session(id: i64, avg: f64, low: f64) -> Session {
        Session {
            id,
            game_id: "steam:1".into(),
            game_name: "Game".into(),
            started_at: id * 1000,
            ended_at: Some(id * 1000 + 600),
            metrics: Some(SessionMetrics {
                frames: 10_000,
                avg_fps: avg,
                p1_low_fps: low,
                p01_low_fps: low * 0.8,
                median_frametime_ms: 1000.0 / avg,
                stutters_per_min: 0.0,
            }),
            capture_note: None,
            excluded: false,
            load_samples: None,
        }
    }

    fn is_regression(a: &SessionAnalysis) -> bool {
        matches!(a.verdict, Verdict::Regression { .. })
    }

    #[test]
    fn needs_history_before_judging() {
        let s: Vec<_> = (1..=3).map(|i| session(i, 100.0, 70.0)).collect();
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        assert!(a.iter().all(|x| matches!(x.verdict, Verdict::InsufficientHistory { .. })));
    }

    #[test]
    fn flags_clear_drop_and_points_to_last_good_session() {
        let mut s: Vec<_> = (1..=4).map(|i| session(i, 100.0 + i as f64, 70.0)).collect();
        s.push(session(5, 75.0, 50.0));
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        assert!(matches!(a[3].verdict, Verdict::Normal { .. }));
        assert!(is_regression(&a[4]));
        assert_eq!(a[4].last_good_at, Some(4000));
    }

    #[test]
    fn low_only_regression_is_detected() {
        let mut s: Vec<_> = (1..=4).map(|i| session(i, 100.0, 70.0)).collect();
        s.push(session(5, 98.0, 45.0)); // average fine, lows collapsed
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        assert!(is_regression(&a[4]));
    }

    #[test]
    fn persistent_regression_keeps_pointing_at_the_original_window() {
        let mut s: Vec<_> = (1..=4).map(|i| session(i, 100.0, 70.0)).collect();
        for i in 5..=10 {
            s.push(session(i, 70.0, 45.0));
        }
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        for x in &a[4..] {
            assert!(is_regression(x), "session {} should stay flagged", x.session_id);
            assert_eq!(x.last_good_at, Some(4000));
        }
    }

    #[test]
    fn noisy_game_is_not_flagged_for_normal_variation() {
        // Swings of +-20 % are normal for this game.
        let avgs = [100.0, 80.0, 120.0, 85.0, 115.0, 82.0];
        let s: Vec<_> = avgs
            .iter()
            .enumerate()
            .map(|(i, a)| session(i as i64 + 1, *a, a * 0.7))
            .collect();
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        assert!(!a.iter().any(is_regression));
    }

    #[test]
    fn small_drop_is_normal_and_gain_is_improvement() {
        let mut s: Vec<_> = (1..=4).map(|i| session(i, 100.0, 70.0)).collect();
        s.push(session(5, 95.0, 66.0));
        s.push(session(6, 130.0, 95.0));
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        assert!(matches!(a[4].verdict, Verdict::Normal { .. }));
        assert!(matches!(a[5].verdict, Verdict::Improvement { .. }));
    }

    #[test]
    fn excluded_sessions_are_ignored_entirely() {
        let mut s: Vec<_> = (1..=4).map(|i| session(i, 100.0, 70.0)).collect();
        // A benchmark run on another map: much faster, marked as not representative.
        let mut odd = session(5, 300.0, 250.0);
        odd.excluded = true;
        s.push(odd);
        s.push(session(6, 101.0, 70.0));
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        assert_eq!(a[4].verdict, Verdict::Excluded);
        assert!(matches!(a[5].verdict, Verdict::Normal { .. }));
        assert!(!a[5].baseline_session_ids.contains(&5));
        assert_eq!(a[5].baseline_session_ids, vec![4, 3, 2, 1]);
    }

    #[test]
    fn sessions_without_metrics_are_skipped() {
        let mut s: Vec<_> = (1..=4).map(|i| session(i, 100.0, 70.0)).collect();
        s[1].metrics = None;
        s.push(session(5, 70.0, 45.0));
        let a = analyze_sessions(&s, &AnalysisConfig::default());
        assert_eq!(a[1].verdict, Verdict::NoData);
        assert!(is_regression(&a[4]));
    }

    fn change(id: i64, at: Timestamp, category: Category, key: &str) -> ChangeEvent {
        ChangeEvent {
            id,
            detected_at: at,
            category,
            key: key.into(),
            label: key.into(),
            kind: ChangeKind::Modified,
            old_value: Some("a".into()),
            new_value: Some("b".into()),
        }
    }

    #[test]
    fn suspects_are_ranked_and_foreign_games_dropped() {
        let changes = vec![
            change(1, 10, Category::Autostart, "HKCU:Tool"),
            change(2, 20, Category::GameBuild, "steam:999"),
            change(3, 30, Category::WindowsUpdate, "KB5066835"),
            change(4, 40, Category::GpuDriver, "RTX"),
            change(5, 50, Category::GameBuild, "steam:1"),
        ];
        let s = rank_suspects(&changes, "steam:1");
        let ids: Vec<_> = s.iter().map(|x| x.change.id).collect();
        assert_eq!(ids, vec![5, 4, 3, 1]);
        assert!(s.iter().all(|x| !x.hint.is_empty()));
    }
}
