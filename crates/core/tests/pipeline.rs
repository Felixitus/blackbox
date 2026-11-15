//! End-to-end test of the core pipeline without touching the real system:
//! scans -> change journal -> sessions -> regression -> ranked suspects.

use std::cell::RefCell;
use std::rc::Rc;

use blackbox_core::analysis::{analyze_sessions, rank_suspects, AnalysisConfig, Verdict};
use blackbox_core::collectors::{run_scan, Collector};
use blackbox_core::frames::metrics_from_frametimes;
use blackbox_core::model::{Category, Fact};
use blackbox_core::store::Store;
use blackbox_core::Result;

const GAME: &str = "steam:730";

/// A collector whose facts the test can change between scans.
struct Scripted {
    category: Category,
    facts: Rc<RefCell<Vec<Fact>>>,
}

impl Collector for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn category(&self) -> Category {
        self.category
    }
    fn collect(&self) -> Result<Vec<Fact>> {
        Ok(self.facts.borrow().clone())
    }
}

fn play(store: &Store, start: i64, frametime_ms: f64, stutter_share: f64) {
    let n = 20_000usize;
    let stutters = (n as f64 * stutter_share) as usize;
    let mut ft = vec![frametime_ms; n - stutters];
    ft.extend(vec![frametime_ms * 4.0; stutters]);
    let metrics = metrics_from_frametimes(&ft, 3000).unwrap();
    let id = store.start_session(GAME, "Counter-Strike 2", start).unwrap();
    store.finish_session(id, start + 1800, Some(&metrics), None).unwrap();
}

#[test]
fn driver_update_between_sessions_becomes_top_suspect() {
    let mut store = Store::open_in_memory().unwrap();

    let gpu = Rc::new(RefCell::new(vec![Fact::new(
        Category::GpuDriver,
        "RX 7900 XTX",
        "AMD Radeon RX 7900 XTX",
        "32.0.1",
    )]));
    let builds = Rc::new(RefCell::new(vec![
        Fact::new(Category::GameBuild, GAME, "Counter-Strike 2", "100"),
        Fact::new(Category::GameBuild, "steam:570", "Dota 2", "500"),
    ]));
    let autostart = Rc::new(RefCell::new(Vec::new()));
    let collectors: Vec<Box<dyn Collector>> = vec![
        Box::new(Scripted { category: Category::GpuDriver, facts: gpu.clone() }),
        Box::new(Scripted { category: Category::GameBuild, facts: builds.clone() }),
        Box::new(Scripted { category: Category::Autostart, facts: autostart.clone() }),
    ];

    // Day 0: baseline scan, then four healthy sessions on days 1-4.
    let day = 86_400;
    assert!(run_scan(&mut store, &collectors, 0).unwrap().changes.is_empty());
    for d in 1..=4 {
        play(&store, d * day, 5.0, 0.0);
        run_scan(&mut store, &collectors, d * day + 3600).unwrap();
    }

    // An old, unrelated change before the last good session must not be blamed.
    // (Detected on day 3, i.e. before the good session of day 4.)
    // Between day 4 and day 5: GPU driver update, Dota 2 patch, new autostart tool.
    gpu.borrow_mut()[0].value = "32.0.2".into();
    builds.borrow_mut()[1].value = "501".into();
    autostart.borrow_mut().push(Fact::new(Category::Autostart, "HKCU:Tool", "Tool", "tool.exe"));
    let report = run_scan(&mut store, &collectors, 4 * day + 7200).unwrap();
    assert_eq!(report.changes.len(), 3);

    // Day 5: clearly slower and stuttering.
    play(&store, 5 * day, 7.0, 0.03);

    let sessions = store.sessions(Some(GAME)).unwrap();
    let analysis = analyze_sessions(&sessions, &AnalysisConfig::default());
    let last = analysis.last().unwrap();
    let Verdict::Regression { avg_change, low_change, .. } = last.verdict else {
        panic!("expected regression, got {:?}", last.verdict);
    };
    assert!(avg_change < -0.25);
    assert!(low_change < -0.5);
    assert_eq!(last.last_good_at, Some(4 * day));

    let changes = store
        .changes_between(last.last_good_at.unwrap(), sessions.last().unwrap().started_at)
        .unwrap();
    let suspects = rank_suspects(&changes, GAME);
    let labels: Vec<_> = suspects.iter().map(|s| s.change.label.as_str()).collect();
    // The Dota 2 patch is irrelevant for CS2 and must be gone.
    assert_eq!(labels, vec!["AMD Radeon RX 7900 XTX", "Tool"]);
    assert_eq!(suspects[0].change.old_value.as_deref(), Some("32.0.1"));
    assert_eq!(suspects[0].change.new_value.as_deref(), Some("32.0.2"));
}
