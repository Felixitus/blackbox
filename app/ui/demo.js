// Sample data, used only when the UI is opened in a plain browser
// (no Tauri backend). The packaged app never reads it.
"use strict";

if (!window.__TAURI__) {
  const day = 86400;
  const now = Math.floor(Date.now() / 1000);
  const metrics = (avg, low, st) => ({
    frames: 180000, avg_fps: avg, p1_low_fps: low, p01_low_fps: low * 0.8,
    median_frametime_ms: 1000 / avg, stutters_per_min: st,
  });
  const base = { sessions: 5, avg_fps: 241, p1_low_fps: 168 };
  const s = (id, daysAgo, m, verdict, extra = {}) => ({
    session: {
      id, game_id: "steam:730", game_name: "Counter-Strike 2",
      started_at: now - daysAgo * day, ended_at: now - daysAgo * day + 5400,
      metrics: m, capture_note: m ? null : "PresentMon not found", excluded: false, load_samples: m ? 600 : null,
    },
    verdict, last_good_at: null, suspects: [], load: [{ name: "Discord.exe", avg_cpu: 2.4, peak_cpu: 5 }], ...extra,
  });
  const driver = {
    id: 3, detected_at: now - 2.4 * day, category: "gpu_driver", key: "RX",
    label: "AMD Radeon RX 7900 XTX", kind: "modified",
    old_value: "32.0.21045.1 (9-12-2026)", new_value: "32.0.32015.7029 (10-18-2026)",
  };
  const kb = {
    id: 2, detected_at: now - 2.6 * day, category: "windows_update", key: "KB5127690",
    label: "KB5127690", kind: "added", old_value: null, new_value: "installed",
  };
  const auto = {
    id: 1, detected_at: now - 2.8 * day, category: "autostart", key: "HKCU:Tool",
    label: "ExampleTool", kind: "added", old_value: null,
    new_value: "\"C:\\Users\\demo\\AppData\\Local\\Programs\\ExampleTool\\ExampleTool.exe\"",
  };
  const suspects = [
    { change: { id: -1, detected_at: now - 2 * day, category: "background_load", key: "MsMpEng.exe", label: "MsMpEng.exe", kind: "modified", old_value: "not active in good sessions", new_value: "avg. 14.0 % CPU, peak 38 %" },
      score: 0.75, hint: "This program used noticeable CPU time during the slow session but not in the good ones. To test, close it before playing or turn off its background activity." },
    { change: driver, score: 0.9, hint: "The graphics driver changed. To test, install the previous version (clean install)." },
    { change: kb, score: 0.7, hint: "A Windows update was installed or removed. To test, uninstall it under Settings › Windows Update › Update history." },
    { change: auto, score: 0.3, hint: "A startup app was added or changed. To test, close it or disable it in Task Manager under \"Startup apps\"." },
  ];
  window.BLACKBOX_DEMO = {
    games: [
      {
        id: "steam:730", name: "Counter-Strike 2", regressed: true,
        sessions: [
          s(1, 9, metrics(238, 165, 0.4), { status: "insufficient_history", have: 0, need: 3 }),
          s(2, 8, metrics(244, 171, 0.2), { status: "insufficient_history", have: 1, need: 3 }),
          s(3, 6, metrics(241, 168, 0.3), { status: "insufficient_history", have: 2, need: 3 }),
          s(4, 5, null, { status: "no_data" }),
          s(5, 4, metrics(246, 170, 0.3), { status: "normal", baseline: base }),
          s(6, 3, metrics(239, 166, 0.5), { status: "normal", baseline: base }),
          s(7, 2, metrics(171, 74, 9.8),
            { status: "regression", baseline: base, avg_change: -0.29, low_change: -0.56 },
            { last_good_at: now - 3 * day, suspects, load: [{ name: "MsMpEng.exe", avg_cpu: 14, peak_cpu: 38 }, { name: "Discord.exe", avg_cpu: 2.5, peak_cpu: 5 }] }),
          s(8, 1, metrics(168, 71, 11.2),
            { status: "regression", baseline: base, avg_change: -0.30, low_change: -0.58 },
            { last_good_at: now - 3 * day, suspects }),
        ],
      },
      {
        id: "steam:1245620", name: "ELDEN RING", regressed: false,
        sessions: [
          { ...s(20, 7, metrics(60, 52, 0.1), { status: "insufficient_history", have: 0, need: 3 }) },
          { ...s(21, 3, metrics(60, 51, 0.2), { status: "insufficient_history", have: 1, need: 3 }) },
        ],
      },
    ],
    changes: [
      { id: 4, detected_at: now - 1.9 * day, category: "game_config", key: "steam:730", label: "Counter-Strike 2: cs2_video.txt", kind: "modified", old_value: null, new_value: "9 of 24 settings changed – possible reset after a game update" },
      driver, kb, auto],
    vault_enabled: true,
    vault: [
      {
        game_id: "steam:730", game_name: "Counter-Strike 2", last_change_at: now - 1.9 * day, reset_suspected: true,
        files: [
          {
            file_id: 1, label: "cs2_video.txt", file_name: "cs2_video.txt", exists: true,
            path: "C:\\Program Files (x86)\\Steam\\userdata\\123\\730\\local\\cfg\\cs2_video.txt",
            snapshots: [
              { id: 12, taken_at: now - 1.9 * day, reason: "changed", size: 1067, game_build: "26598758", current: true,
                summary: { changed: 9, total: 24, binary: false, build_changed: true, reset_likely: true } },
              { id: 7, taken_at: now - 6 * day, reason: "changed", size: 1109, game_build: "26511101", current: false,
                summary: { changed: 1, total: 24, binary: false, build_changed: false, reset_likely: false } },
              { id: 2, taken_at: now - 9 * day, reason: "baseline", size: 1109, game_build: "26511101", current: false,
                summary: { changed: 0, total: 0, binary: false, build_changed: false, reset_likely: false } },
            ],
          },
          {
            file_id: 2, label: "cs2_user_keys_0_slot0.vcfg", file_name: "cs2_user_keys_0_slot0.vcfg", exists: true,
            path: "C:\\Program Files (x86)\\Steam\\userdata\\123\\730\\local\\cfg\\cs2_user_keys_0_slot0.vcfg",
            snapshots: [
              { id: 3, taken_at: now - 9 * day, reason: "baseline", size: 404, game_build: "26511101", current: true,
                summary: { changed: 0, total: 0, binary: false, build_changed: false, reset_likely: false } },
            ],
          },
        ],
      },
      {
        game_id: "steam:2073850", game_name: "THE FINALS", last_change_at: null, reset_suspected: false,
        files: [{
          file_id: 3, label: "GameUserSettings.ini", file_name: "GameUserSettings.ini", exists: true,
          path: "C:\\Users\\demo\\AppData\\Local\\Discovery\\Saved\\Config\\WindowsClient\\GameUserSettings.ini",
          snapshots: [{ id: 4, taken_at: now - 9 * day, reason: "baseline", size: 3120, game_build: "1", current: true,
            summary: { changed: 0, total: 0, binary: false, build_changed: false, reset_likely: false } }],
        }],
      },
    ],
    state: [
      { category: "os_build", key: "windows", label: "Windows", value: "25H2 (Build 26200.9711)" },
      { category: "gpu_driver", key: "RX", label: "AMD Radeon RX 7900 XTX", value: "32.0.32015.7029 (10-18-2026)" },
      { category: "power_plan", key: "active", label: "Active power plan", value: "Ultimate Performance" },
      { category: "system_setting", key: "game_mode", label: "Game Mode", value: "default" },
    ],
    status: { active_game: null, capturing: false, last_scan_at: now - 240, error: null },
    presentmon_path: null,
    presentmon_ok: true,
    presentmon_bundled: "C:\\Users\\demo\\AppData\\Local\\Blackbox\\PresentMon.exe",
    autostart: false,
    notifications: true,
    language: "en",
    language_setting: "auto",
    locale: "en-US",
    capture_rights: "none",
    category_labels: {
      os_build: "Windows build", windows_update: "Windows update", gpu_driver: "Graphics driver",
      audio_driver: "Audio driver", bios: "BIOS/UEFI", power_plan: "Power plan",
      system_setting: "System setting", autostart: "Startup app", game_build: "Game version",
      game_config: "Game settings", background_load: "Background load",
    },
    database: "C:\\Users\\demo\\AppData\\Local\\Blackbox\\data\\blackbox.db",
    log_dir: "C:\\Users\\demo\\AppData\\Local\\Blackbox\\data\\logs",
  };
}

if (!window.__TAURI__) {
  window.BLACKBOX_DEMO_DIFF = (id) => (id === 12
    ? {
        snapshot_id: 12, previous_id: 7, file_name: "cs2_video.txt", binary: false, total: 24,
        changes: [
          { key: "setting.defaultres", old: "2560", new: "1920", class: "display" },
          { key: "setting.defaultresheight", old: "1440", new: "1080", class: "display" },
          { key: "setting.fullscreen", old: "1", new: "0", class: "display" },
          { key: "setting.mat_vsync", old: "0", new: "1", class: "graphics" },
          { key: "setting.msaa_samples", old: "4", new: "0", class: "graphics" },
          { key: "setting.r_csgo_fsr_upsample", old: "0", new: "1", class: "graphics" },
          { key: "setting.shaderquality", old: "1", new: "0", class: "graphics" },
          { key: "setting.r_texturefilteringquality", old: "5", new: "1", class: "graphics" },
          { key: "setting.refreshrate_numerator", old: "240", new: "60", class: "display" },
        ],
      }
    : { snapshot_id: id, previous_id: null, file_name: "file", binary: false, total: 1, changes: [{ key: "setting.example", old: null, new: "1", class: "personal" }] });
}
if (!window.__TAURI__) {
  window.BLACKBOX_DEMO_IMPORT = (apply) => [
    { game_id: "steam:730", game_name: "Counter-Strike 2", label: "cs2_user_convars_0_slot0.vcfg", changed: ["convars/sensitivity", "convars/volume", "convars/cl_crosshairsize"], applied: apply },
    { game_id: "steam:2073850", game_name: "THE FINALS", label: "GameUserSettings.ini", changed: [], applied: false },
  ];
}