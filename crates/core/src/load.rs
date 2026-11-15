//! Background load during play sessions.
//!
//! While a game runs, the CPU share of every other process is sampled with
//! each process-list poll. A program that eats noticeable CPU time only in
//! the slow session (an update service, a browser tab, an indexer) is a
//! suspect even when no system change was journaled.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Processes below this average share of the whole CPU are not stored.
pub const MIN_STORED_CPU: f64 = 1.0;
/// At most this many processes are stored per session.
pub const MAX_STORED: usize = 12;
/// A process must average at least this much CPU in the slow session.
pub const MIN_SUSPECT_CPU: f64 = 2.0;

/// Processes that are part of measuring or of Windows itself and are never
/// reported (lower-case executable names).
const IGNORED: &[&str] = &[
    "system idle process", "idle", "system", "registry", "memory compression", "secure system",
    "blackbox-app.exe", "blackbox.exe", "presentmon.exe",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessLoad {
    pub name: String,
    /// Average share of the whole CPU over the session, in percent.
    pub avg_cpu: f64,
    /// Highest single sample, in percent of the whole CPU.
    pub peak_cpu: f64,
}

/// Accumulates CPU samples of one session.
#[derive(Debug, Default)]
pub struct LoadTracker {
    samples: u32,
    /// lower-case name -> (display name, sum, peak)
    totals: HashMap<String, (String, f64, f64)>,
}

impl LoadTracker {
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Add one sample: `(executable name, CPU share in percent of the whole
    /// CPU)`. Several processes with the same name (browser tabs) are summed.
    pub fn add_sample<'a>(&mut self, processes: impl IntoIterator<Item = (&'a str, f64)>, excluded: &[String]) {
        let mut this: HashMap<String, (String, f64)> = HashMap::new();
        for (name, cpu) in processes {
            if !cpu.is_finite() || cpu <= 0.0 {
                continue;
            }
            let key = name.to_lowercase();
            if IGNORED.contains(&key.as_str()) || excluded.iter().any(|e| e.eq_ignore_ascii_case(name)) {
                continue;
            }
            this.entry(key).or_insert_with(|| (name.to_string(), 0.0)).1 += cpu;
        }
        for (key, (name, cpu)) in this {
            let e = self.totals.entry(key).or_insert((name, 0.0, 0.0));
            e.1 += cpu;
            e.2 = e.2.max(cpu);
        }
        self.samples += 1;
    }

    /// Processes worth keeping, highest average first.
    pub fn finish(&self) -> Vec<ProcessLoad> {
        if self.samples == 0 {
            return Vec::new();
        }
        let mut out: Vec<ProcessLoad> = self
            .totals
            .values()
            .map(|(name, sum, peak)| ProcessLoad {
                name: name.clone(),
                avg_cpu: sum / f64::from(self.samples),
                peak_cpu: *peak,
            })
            .filter(|p| p.avg_cpu >= MIN_STORED_CPU)
            .collect();
        out.sort_by(|a, b| b.avg_cpu.total_cmp(&a.avg_cpu).then_with(|| a.name.cmp(&b.name)));
        out.truncate(MAX_STORED);
        out
    }
}

/// A process that loaded the CPU noticeably more in the slow session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoadSuspect {
    pub process: ProcessLoad,
    /// Highest average of the same process in the good sessions (0 = absent).
    pub baseline_max: f64,
}

/// Compare the slow session with the good ones. Only good sessions that
/// recorded background load count; without any, nothing is reported.
pub fn background_suspects(slow: &[ProcessLoad], good: &[Vec<ProcessLoad>]) -> Vec<LoadSuspect> {
    if good.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<LoadSuspect> = slow
        .iter()
        .filter(|p| p.avg_cpu >= MIN_SUSPECT_CPU)
        .filter_map(|p| {
            let baseline_max = good
                .iter()
                .flat_map(|s| s.iter())
                .filter(|q| q.name.eq_ignore_ascii_case(&p.name))
                .map(|q| q.avg_cpu)
                .fold(0.0, f64::max);
            let new = baseline_max < 0.5;
            let much_higher = p.avg_cpu >= baseline_max * 3.0 && p.avg_cpu - baseline_max >= MIN_SUSPECT_CPU;
            (new || much_higher).then(|| LoadSuspect { process: p.clone(), baseline_max })
        })
        .collect();
    out.sort_by(|a, b| b.process.avg_cpu.total_cmp(&a.process.avg_cpu));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(name: &str, avg: f64) -> ProcessLoad {
        ProcessLoad { name: name.into(), avg_cpu: avg, peak_cpu: avg }
    }

    #[test]
    fn tracker_averages_sums_and_filters() {
        let mut t = LoadTracker::default();
        let game = vec!["Game.exe".to_string()];
        t.add_sample([("chrome.exe", 2.0), ("chrome.exe", 3.0), ("Game.exe", 40.0), ("Idle", 50.0)], &game);
        t.add_sample([("chrome.exe", 1.0), ("updater.exe", 9.0), ("tiny.exe", 0.5)], &game);
        t.add_sample([("CHROME.EXE", 0.0), ("bad.exe", f64::NAN)], &game);
        assert_eq!(t.samples(), 3);
        let out = t.finish();
        let names: Vec<_> = out.iter().map(|p| p.name.as_str()).collect();
        // updater: 9/3 = 3.0, chrome: (5+1)/3 = 2.0, tiny: 0.17 (dropped)
        assert_eq!(names, vec!["updater.exe", "chrome.exe"]);
        assert!((out[1].avg_cpu - 2.0).abs() < 1e-9);
        assert!((out[1].peak_cpu - 5.0).abs() < 1e-9);
    }

    #[test]
    fn tracker_keeps_only_the_top_entries() {
        let mut t = LoadTracker::default();
        let names: Vec<String> = (0..20).map(|i| format!("p{i:02}.exe")).collect();
        t.add_sample(names.iter().enumerate().map(|(i, n)| (n.as_str(), 2.0 + i as f64)), &[]);
        let out = t.finish();
        assert_eq!(out.len(), MAX_STORED);
        assert_eq!(out[0].name, "p19.exe");
        assert!(LoadTracker::default().finish().is_empty());
    }

    #[test]
    fn suspects_are_new_or_much_higher_processes() {
        let good = vec![
            vec![load("discord.exe", 3.0), load("search.exe", 1.0)],
            vec![load("discord.exe", 3.5)],
        ];
        let slow = vec![
            load("discord.exe", 4.0),   // normal level
            load("search.exe", 9.0),    // 9x higher
            load("updater.exe", 6.0),   // new
            load("helper.exe", 1.5),    // too small
        ];
        let s = background_suspects(&slow, &good);
        let names: Vec<_> = s.iter().map(|x| x.process.name.as_str()).collect();
        assert_eq!(names, vec!["search.exe", "updater.exe"]);
        assert_eq!(s[0].baseline_max, 1.0);
        assert_eq!(s[1].baseline_max, 0.0);
    }

    #[test]
    fn no_comparison_without_good_sessions() {
        assert!(background_suspects(&[load("x.exe", 50.0)], &[]).is_empty());
    }
}
