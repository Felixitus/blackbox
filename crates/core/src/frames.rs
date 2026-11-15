//! Frame time statistics and PresentMon CSV import.

use std::collections::HashMap;
use std::io::Read;

use crate::error::{Error, Result};
use crate::model::SessionMetrics;

/// Frames longer than this are treated as loading screens / alt-tab pauses
/// and ignored, so they do not dominate the percentiles.
pub const MAX_PLAUSIBLE_FRAMETIME_MS: f64 = 1000.0;

/// A frame counts as a stutter when it takes this many times the median.
pub const STUTTER_FACTOR: f64 = 2.5;

/// Value at quantile `q` (0..=1) of an ascending sorted slice (nearest rank).
fn quantile(sorted: &[f64], q: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Compute session metrics from raw frame times in milliseconds.
/// Returns `None` when fewer than `min_frames` usable frames remain.
pub fn metrics_from_frametimes(frametimes_ms: &[f64], min_frames: usize) -> Option<SessionMetrics> {
    let mut ft: Vec<f64> = frametimes_ms
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= MAX_PLAUSIBLE_FRAMETIME_MS)
        .collect();
    if ft.is_empty() || ft.len() < min_frames {
        return None;
    }
    ft.sort_by(|a, b| a.total_cmp(b));

    let total_ms: f64 = ft.iter().sum();
    let median = quantile(&ft, 0.5);
    let stutter_limit = median * STUTTER_FACTOR;
    let stutters = ft.iter().rev().take_while(|v| **v > stutter_limit).count();

    Some(SessionMetrics {
        frames: ft.len() as u64,
        avg_fps: ft.len() as f64 * 1000.0 / total_ms,
        p1_low_fps: 1000.0 / quantile(&ft, 0.99),
        p01_low_fps: 1000.0 / quantile(&ft, 0.999),
        median_frametime_ms: median,
        stutters_per_min: stutters as f64 / (total_ms / 60_000.0),
    })
}

/// Read frame times from a PresentMon CSV.
///
/// Supports both the 1.x column (`MsBetweenPresents`) and the 2.x column
/// (`FrameTime`). If `exe_names` is non-empty only rows of those
/// applications are used; among the candidates the application with the
/// most frames wins (a game and its launcher may both present frames).
pub fn read_presentmon_csv<R: Read>(reader: R, exe_names: &[String]) -> Result<Vec<f64>> {
    let mut csv = csv::ReaderBuilder::new().flexible(true).from_reader(reader);
    let headers = csv.headers()?.clone();
    let find = |names: &[&str]| {
        headers
            .iter()
            .position(|h| names.iter().any(|n| h.trim().eq_ignore_ascii_case(n)))
    };
    let app_col = find(&["Application"])
        .ok_or_else(|| Error::Capture("CSV has no `Application` column".into()))?;
    let ft_col = find(&["MsBetweenPresents", "FrameTime"]).ok_or_else(|| {
        Error::Capture("CSV has neither `MsBetweenPresents` nor `FrameTime`".into())
    })?;

    let wanted: Vec<String> = exe_names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let mut per_app: HashMap<String, Vec<f64>> = HashMap::new();
    for record in csv.records() {
        let Ok(record) = record else {
            continue; // a truncated last line is expected when capture is stopped
        };
        let (Some(app), Some(ft)) = (record.get(app_col), record.get(ft_col)) else {
            continue;
        };
        let app = app.trim().to_ascii_lowercase();
        if !wanted.is_empty() && !wanted.contains(&app) {
            continue;
        }
        if let Ok(ft) = ft.trim().parse::<f64>() {
            per_app.entry(app).or_default().push(ft);
        }
    }
    Ok(per_app
        .into_values()
        .max_by_key(|v| v.len())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_60_fps() {
        let ft = vec![1000.0 / 60.0; 6000];
        let m = metrics_from_frametimes(&ft, 1000).unwrap();
        assert!((m.avg_fps - 60.0).abs() < 0.01);
        assert!((m.p1_low_fps - 60.0).abs() < 0.01);
        assert_eq!(m.stutters_per_min, 0.0);
        assert_eq!(m.frames, 6000);
    }

    #[test]
    fn stutters_lower_the_lows_but_barely_the_average() {
        // 10 000 frames at 10 ms, 2 % of them at 40 ms.
        let mut ft = vec![10.0; 9800];
        ft.extend(vec![40.0; 200]);
        let m = metrics_from_frametimes(&ft, 1000).unwrap();
        assert!(m.avg_fps > 90.0 && m.avg_fps < 100.0);
        assert!((m.p1_low_fps - 25.0).abs() < 0.01);
        assert!(m.stutters_per_min > 100.0);
    }

    #[test]
    fn ignores_implausible_frames_and_enforces_minimum() {
        let mut ft = vec![10.0; 500];
        ft.push(30_000.0); // loading screen
        ft.push(f64::NAN);
        ft.push(-1.0);
        let m = metrics_from_frametimes(&ft, 100).unwrap();
        assert_eq!(m.frames, 500);
        assert!(metrics_from_frametimes(&ft, 1000).is_none());
        assert!(metrics_from_frametimes(&[], 0).is_none());
    }

    #[test]
    fn reads_v1_csv_and_picks_requested_app() {
        let data = "Application,ProcessID,MsBetweenPresents\n\
                    game.exe,1,10.0\n\
                    Game.exe,1,12.0\n\
                    chrome.exe,2,16.0\n\
                    chrome.exe,2,16.0\n\
                    chrome.exe,2,16.0\n";
        let ft = read_presentmon_csv(data.as_bytes(), &["GAME.EXE".to_string()]).unwrap();
        assert_eq!(ft, vec![10.0, 12.0]);
        // Without a filter the busiest application wins.
        let ft = read_presentmon_csv(data.as_bytes(), &[]).unwrap();
        assert_eq!(ft.len(), 3);
    }

    #[test]
    fn reads_v2_csv_and_tolerates_truncation() {
        let data = "Application,ProcessID,SwapChainAddress,FrameTime\n\
                    game.exe,1,0x1,8.3\n\
                    game.exe,1,0x1,NA\n\
                    game.exe,1,0x1,8.5\n\
                    game.exe,1";
        let ft = read_presentmon_csv(data.as_bytes(), &[]).unwrap();
        assert_eq!(ft, vec![8.3, 8.5]);
    }

    #[test]
    fn rejects_csv_without_required_columns() {
        assert!(read_presentmon_csv("a,b\n1,2\n".as_bytes(), &[]).is_err());
    }
}
