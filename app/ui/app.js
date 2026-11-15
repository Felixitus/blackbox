// Blackbox UI. Renders the dashboard object delivered by the backend.
// All dynamic text goes through textContent, never innerHTML.
// Every text exists in German and English: tr("Deutsch", "English").
"use strict";

const tauri = window.__TAURI__;
const SVG_NS = "http://www.w3.org/2000/svg";

let data = null;
let view = { kind: "auto" }; // auto | game | vault | changes | state | settings

const lang = () => (data && data.language === "en" ? "en" : "de");
const tr = (de, en) => (lang() === "en" ? en : de);
const locale = () => (data && data.locale) || (lang() === "en" ? "en-US" : "de-DE");

const backend = tauri
  ? {
      dashboard: () => tauri.core.invoke("dashboard"),
      scanNow: () => tauri.core.invoke("scan_now"),
      setPresentmon: (path) => tauri.core.invoke("set_presentmon", { path }),
      vaultDiff: (snapshotId) => tauri.core.invoke("vault_diff", { snapshotId }),
      vaultRestore: (snapshotId, personalOnly) => tauri.core.invoke("vault_restore", { snapshotId, personalOnly }),
      vaultExport: async () => {
        const path = await tauri.dialog.save({
          title: tr("Einstellungsprofil exportieren", "Export settings profile"),
          defaultPath: `${tr("Blackbox-Profil", "Blackbox-profile")}-${new Date().toISOString().slice(0, 10)}.json`,
          filters: [{ name: tr("Blackbox-Profil", "Blackbox profile"), extensions: ["json"] }],
        });
        return path ? { path, files: await tauri.core.invoke("vault_export", { path }) } : null;
      },
      vaultPickProfile: () => tauri.dialog.open({
        title: tr("Einstellungsprofil importieren", "Import settings profile"), multiple: false, directory: false,
        filters: [{ name: tr("Blackbox-Profil", "Blackbox profile"), extensions: ["json"] }],
      }),
      vaultImport: (path, apply) => tauri.core.invoke("vault_import", { path, apply }),
      setAutostart: (enabled) => tauri.core.invoke("set_autostart", { enabled }),
      requestCaptureRights: () => tauri.core.invoke("request_capture_rights"),
      setSessionExcluded: (sessionId, excluded) => tauri.core.invoke("set_session_excluded", { sessionId, excluded }),
      sessionReport: (sessionId) => tauri.core.invoke("session_report", { sessionId }),
      setNotifications: (enabled) => tauri.core.invoke("set_notifications", { enabled }),
      setLanguage: (language) => tauri.core.invoke("set_language", { language }),
      onUpdate: (fn) => tauri.event.listen("blackbox-updated", fn),
    }
  : {
      // Opened in a plain browser: show sample data for design work.
      dashboard: async () => window.BLACKBOX_DEMO,
      scanNow: async () => {},
      setPresentmon: async () => {},
      setAutostart: async (enabled) => { window.BLACKBOX_DEMO.autostart = enabled; },
      requestCaptureRights: async () => { throw new Error(tr("In der Vorschau nicht verfügbar.", "Not available in the preview.")); },
      setSessionExcluded: async (id, excluded) => {
        for (const g of window.BLACKBOX_DEMO.games) for (const r of g.sessions) if (r.session.id === id) r.session.excluded = excluded;
      },
      sessionReport: async (id) => `## Blackbox – Session #${id} (${tr("Vorschau-Daten", "preview data")})\n`,
      setNotifications: async (enabled) => { window.BLACKBOX_DEMO.notifications = enabled; },
      setLanguage: async (language) => {
        const d = window.BLACKBOX_DEMO;
        d.language_setting = language;
        d.language = language === "auto" ? (navigator.language.startsWith("de") ? "de" : "en") : language;
        d.locale = d.language === "en" ? "en-US" : "de-DE";
      },
      vaultDiff: async (id) => window.BLACKBOX_DEMO_DIFF(id),
      vaultRestore: async () => { throw new Error(tr("In der Vorschau nicht verfügbar.", "Not available in the preview.")); },
      vaultExport: async () => { throw new Error(tr("In der Vorschau nicht verfügbar.", "Not available in the preview.")); },
      vaultPickProfile: async () => "C:\\Users\\demo\\Blackbox-profile.json",
      vaultImport: async (path, apply) => window.BLACKBOX_DEMO_IMPORT(apply),
      onUpdate: () => {},
    };

// ---- helpers ---------------------------------------------------------------

function h(tag, props, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return el;
}

function svg(tag, attrs, ...children) {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) el.setAttribute(k, v);
  for (const c of children) el.append(c.nodeType ? c : document.createTextNode(String(c)));
  return el;
}

const fmtWith = (opts) => (ts) => new Intl.DateTimeFormat(locale(), opts).format(new Date(ts * 1000));
const fmtDate = fmtWith({ day: "2-digit", month: "2-digit", year: "numeric" });
const fmtTime = fmtWith({ hour: "2-digit", minute: "2-digit" });
const fmtDay = fmtWith({ weekday: "long", day: "2-digit", month: "long", year: "numeric" });
const fmtDateTime = (ts) => `${fmtDate(ts)}, ${fmtTime(ts)}`;
const fmtPct = (v) => `${v >= 0 ? "+" : "−"}${Math.abs(v * 100).toFixed(0)} %`;
const fmtFps = (v) => v.toFixed(0);
const avgLabel = () => tr("Ø FPS", "Avg. FPS");
const lowLabel = () => tr("1%-Low", "1% low");

function fmtDuration(s) {
  if (s.ended_at == null) return tr("läuft", "running");
  const min = Math.round((s.ended_at - s.started_at) / 60);
  return min >= 60 ? `${Math.floor(min / 60)} h ${min % 60} min` : `${min} min`;
}

function catLabel(c) {
  return (data.category_labels && data.category_labels[c]) || c;
}

function changeDetail(c) {
  if (c.kind === "added") return h("span", { class: "mono" }, tr("neu: ", "new: "), c.new_value || "");
  if (c.kind === "removed") return h("span", { class: "mono" }, tr("entfernt (war: ", "removed (was: "), c.old_value || "", ")");
  if (c.old_value == null) return h("span", {}, c.new_value || "");
  return h("span", { class: "mono" },
    h("span", { class: "old" }, c.old_value || "?"),
    h("span", { class: "arrow" }, "  →  "),
    c.new_value || "?");
}

function verdictBadge(v) {
  switch (v.status) {
    case "regression":
      return h("span", { class: "verdict bad" },
        tr(`Einbruch (Ø ${fmtPct(v.avg_change)}, 1%-Low ${fmtPct(v.low_change)})`,
          `Drop (avg. ${fmtPct(v.avg_change)}, 1% low ${fmtPct(v.low_change)})`));
    case "improvement":
      return h("span", { class: "verdict good" }, tr(`besser (Ø ${fmtPct(v.avg_change)})`, `better (avg. ${fmtPct(v.avg_change)})`));
    case "normal":
      return h("span", { class: "verdict" }, tr("normal", "normal"));
    case "insufficient_history":
      return h("span", { class: "verdict muted" }, tr(`Basis wird aufgebaut (${v.have}/${v.need})`, `building baseline (${v.have}/${v.need})`));
    case "excluded":
      return h("span", { class: "verdict muted" }, tr("ausgeschlossen", "excluded"));
    default:
      return h("span", { class: "verdict muted" }, tr("keine Frame-Daten", "no frame data"));
  }
}

// ---- top bar & sidebar -----------------------------------------------------

/// Static texts in index.html carry both languages: data-t="Deutsch|English".
function renderStaticTexts() {
  document.documentElement.lang = lang();
  for (const el of document.querySelectorAll("[data-t]")) {
    const [de, en] = el.dataset.t.split("|");
    el.textContent = tr(de, en);
  }
}

function renderStatus() {
  const st = data.status;
  const pills = [];
  if (st.error) pills.push(h("span", { class: "pill bad", title: st.error }, tr("Fehler im Hintergrunddienst", "Background service error")));
  if (st.active_game) {
    pills.push(h("span", { class: "pill live" }, tr("● Aufzeichnung: ", "● Recording: "), st.active_game,
      st.capturing ? "" : tr(" (ohne Frame-Messung)", " (without frame capture)")));
  } else if (!st.error) {
    pills.push(h("span", { class: "pill live" }, tr("● Überwachung aktiv", "● Monitoring active")));
  }
  if (st.last_scan_at) pills.push(h("span", { class: "pill" }, tr("Letzter Scan ", "Last scan "), fmtTime(st.last_scan_at)));
  if (!data.presentmon_ok) {
    pills.push(h("span", {
      class: "pill warn clickable",
      title: tr("Ohne PresentMon werden Sessions ohne FPS-Daten erfasst.", "Without PresentMon, sessions are recorded without FPS data."),
      onclick: () => show({ kind: "settings" }),
    }, tr("Frame-Messung einrichten", "Set up frame capture")));
  }
  document.getElementById("status").replaceChildren(...pills);
}

function renderSidebar() {
  renderStaticTexts();
  const list = document.getElementById("games");
  if (!data.games.length) {
    list.replaceChildren(h("div", { class: "nav-empty" }, tr("Noch keine Sessions", "No sessions yet")));
  } else {
    list.replaceChildren(...data.games.map((g) => {
      const hasData = g.sessions.some((s) => s.session.metrics);
      const dot = g.regressed ? "bad" : hasData ? "good" : "";
      const active = view.kind === "game" && view.id === g.id;
      return h("button", {
        class: `nav-item${active ? " active" : ""}`,
        onclick: () => show({ kind: "game", id: g.id }),
      }, h("span", { class: `dot ${dot}` }), h("span", { class: "name" }, g.name));
    }));
  }
  for (const btn of document.querySelectorAll(".nav-item[data-view]")) {
    btn.classList.toggle("active", view.kind === btn.dataset.view);
  }
  const anyReset = (data.vault || []).some((g) => g.reset_suspected);
  document.getElementById("vault-dot").className = `dot ${anyReset ? "bad" : (data.vault || []).length ? "good" : ""}`;
}

// ---- game view -------------------------------------------------------------

function chart(game) {
  const points = game.sessions.filter((s) => s.session.metrics && !s.session.excluded);
  if (points.length < 2) {
    return h("div", { class: "card" }, h("p", { class: "note" },
      tr("Der Verlauf erscheint ab zwei Sessions mit Frame-Daten.", "The history appears after two sessions with frame data.")));
  }
  const W = 900, H = 240, L = 44, R = 14, T = 12, B = 26;
  const max = Math.max(...points.map((p) => p.session.metrics.avg_fps)) * 1.1;
  const x = (i) => L + (i * (W - L - R)) / (points.length - 1);
  const y = (v) => T + (1 - v / max) * (H - T - B);
  const root = svg("svg", { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none" });

  for (let i = 0; i <= 4; i++) {
    const v = (max / 4) * i;
    root.append(
      svg("line", { x1: L, x2: W - R, y1: y(v), y2: y(v), stroke: "#2a2f3a", "stroke-width": 1 }),
      svg("text", { x: L - 8, y: y(v) + 4, fill: "#8b92a1", "font-size": 11, "text-anchor": "end" }, v.toFixed(0)));
  }

  // Marker between two sessions when the journal has changes in that window.
  for (let i = 1; i < points.length; i++) {
    const from = points[i - 1].session.started_at, to = points[i].session.started_at;
    const hits = data.changes.filter((c) => c.detected_at > from && c.detected_at <= to &&
      (c.category !== "game_build" || c.key === game.id));
    if (!hits.length) continue;
    const mx = (x(i - 1) + x(i)) / 2;
    const marker = svg("g", {},
      svg("line", { x1: mx, x2: mx, y1: T, y2: H - B, stroke: "#5aa9ff", "stroke-width": 1.5, "stroke-dasharray": "4 4" }),
      svg("circle", { cx: mx, cy: T + 4, r: 4, fill: "#5aa9ff" }),
      svg("title", {}, hits.map((c) => `${catLabel(c.category)}: ${c.label}`).join("\n")));
    root.append(marker);
  }

  const line = (key, color, width) => svg("polyline", {
    points: points.map((p, i) => `${x(i)},${y(p.session.metrics[key])}`).join(" "),
    fill: "none", stroke: color, "stroke-width": width, "stroke-linejoin": "round",
  });
  root.append(line("p1_low_fps", "#8b92a1", 2), line("avg_fps", "#ff8a1f", 2.5));

  points.forEach((p, i) => {
    const m = p.session.metrics;
    const bad = p.verdict.status === "regression";
    root.append(svg("circle", {
      cx: x(i), cy: y(m.avg_fps), r: bad ? 6 : 4,
      fill: bad ? "#ff5a46" : "#ff8a1f", stroke: "#171a21", "stroke-width": 2,
    }, svg("title", {}, `${fmtDateTime(p.session.started_at)}\n${avgLabel()} ${fmtFps(m.avg_fps)} · ${lowLabel()} ${fmtFps(m.p1_low_fps)}`)));
  });
  const first = points[0].session.started_at, last = points[points.length - 1].session.started_at;
  root.append(
    svg("text", { x: L, y: H - 6, fill: "#8b92a1", "font-size": 11 }, fmtDate(first)),
    svg("text", { x: W - R, y: H - 6, fill: "#8b92a1", "font-size": 11, "text-anchor": "end" }, fmtDate(last)));

  const key = (color, text) => h("span", {}, h("i", { style: `background:${color}` }), text);
  return h("div", { class: "chart" },
    h("div", { class: "legend" },
      key("#ff8a1f", avgLabel()), key("#8b92a1", lowLabel()),
      key("#ff5a46", tr("Einbruch", "Drop")), key("#5aa9ff", tr("Systemänderung dazwischen", "System change in between"))),
    root);
}

function regressionCard(game, r) {
  const m = r.session.metrics, v = r.verdict;
  const when = fmtDateTime(r.session.started_at);
  const card = h("div", { class: "card alert" },
    h("h3", {}, tr(`Leistungseinbruch am ${when}`, `Performance drop on ${when}`)),
    h("div", { class: "meta" },
      tr(`Ø ${fmtFps(m.avg_fps)} statt ${fmtFps(v.baseline.avg_fps)} FPS (${fmtPct(v.avg_change)}) · `,
        `Avg. ${fmtFps(m.avg_fps)} instead of ${fmtFps(v.baseline.avg_fps)} FPS (${fmtPct(v.avg_change)}) · `),
      tr(`1%-Low ${fmtFps(m.p1_low_fps)} statt ${fmtFps(v.baseline.p1_low_fps)} (${fmtPct(v.low_change)})`,
        `1% low ${fmtFps(m.p1_low_fps)} instead of ${fmtFps(v.baseline.p1_low_fps)} (${fmtPct(v.low_change)})`)));
  const msg = h("span", { class: "msg" });
  const copy = h("button", { class: "btn small" }, tr("Bericht kopieren", "Copy report"));
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(await backend.sessionReport(r.session.id));
      msg.className = "msg good";
      msg.textContent = tr("In die Zwischenablage kopiert – ohne Benutzernamen und Pfade, fertig für Forum oder Support.",
        "Copied to the clipboard – without user names or paths, ready for forums or support.");
    } catch (e) { msg.className = "msg bad"; msg.textContent = String(e); }
  });
  const actions = h("div", { class: "card-actions" }, copy, excludeButton(r, tr("Nicht repräsentativ", "Not representative")), msg);
  const since = r.last_good_at ? fmtDateTime(r.last_good_at) : "–";
  if (!r.suspects.length) {
    card.append(h("p", { class: "note" },
      tr(`Seit der letzten guten Session (${since}) wurde keine Änderung erfasst, und kein Programm fiel durch zusätzliche CPU-Last auf. `,
        `No change was recorded since the last good session (${since}), and no program stood out with extra CPU load. `),
      tr("Mögliche Ursachen außerhalb der Erfassung: Temperatur oder eine anspruchsvollere Spielszene. War die Session untypisch (andere Karte, Benchmark), markiere sie als nicht repräsentativ.",
        "Possible causes outside of what is recorded: temperature or a more demanding game scene. If the session was untypical (another map, a benchmark), mark it as not representative.")),
      actions);
    return card;
  }
  card.append(
    h("p", { class: "note" }, tr(`Änderungen seit der letzten guten Session (${since}), wahrscheinlichste zuerst:`,
      `Changes since the last good session (${since}), most likely first:`)),
    h("ol", { class: "suspects" }, r.suspects.map((s) => h("li", {},
      h("div", { class: "what" }, h("span", { class: "tag" }, catLabel(s.change.category)),
        s.change.label, " ", changeDetail(s.change)),
      h("div", { class: "hint" }, s.hint)))),
    h("p", { class: "note" }, tr("Die Liste zeigt zeitliche Zusammenhänge, keinen Beweis. Sicherheit bringt nur der Gegentest.",
      "The list shows correlations in time, not proof. Only undoing a change and testing again brings certainty.")),
    actions);
  return card;
}

function excludeButton(r, label) {
  const excluded = !!r.session.excluded;
  const btn = h("button", { class: "btn small" }, excluded ? tr("Wieder einbeziehen", "Include again") : label || tr("Ausschließen", "Exclude"));
  btn.title = excluded
    ? tr("Die Session zählt wieder für den Vergleich.", "The session is used for comparison again.")
    : tr("Untypische Session (andere Karte, Benchmark, kurz angespielt): zählt dann nicht für den Vergleich.",
      "Untypical session (another map, a benchmark, a short try): it is then not used for comparison.");
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    try {
      await backend.setSessionExcluded(r.session.id, !excluded);
      await refresh();
    } catch (e) { btn.disabled = false; window.alert(String(e)); }
  });
  return btn;
}

function loadText(r) {
  if (!r.load || !r.load.length) return r.session.load_samples == null ? "–" : tr("unauffällig", "unremarkable");
  return r.load.slice(0, 2).map((p) => `${p.name} ${p.avg_cpu.toFixed(0)} %`).join(", ");
}

function gameView(game) {
  const withData = game.sessions.filter((s) => s.session.metrics);
  const latest = withData[withData.length - 1];
  const out = [h("h1", {}, game.name),
    h("p", { class: "sub" }, tr(`${game.sessions.length} Session(s), ${withData.length} mit Frame-Daten`,
      `${game.sessions.length} session(s), ${withData.length} with frame data`))];

  if (latest) {
    const m = latest.session.metrics, v = latest.verdict;
    const delta = (change) => change == null ? null
      : h("div", { class: `delta ${change < -0.05 ? "bad" : change > 0.05 ? "good" : ""}` }, tr(`${fmtPct(change)} zur Basis`, `${fmtPct(change)} vs. baseline`));
    const tile = (label, value, extra) => h("div", { class: "tile" },
      h("div", { class: "label" }, label), h("div", { class: "value" }, value), extra);
    out.push(h("div", { class: "tiles" },
      tile(tr("Ø FPS (letzte Session)", "Avg. FPS (last session)"), fmtFps(m.avg_fps), delta(v.avg_change)),
      tile(lowLabel(), fmtFps(m.p1_low_fps), delta(v.low_change)),
      tile(tr("Ruckler pro Minute", "Stutters per minute"), m.stutters_per_min.toFixed(1)),
      tile(tr("Basis Ø FPS", "Baseline avg. FPS"), v.baseline ? fmtFps(v.baseline.avg_fps) : "–",
        h("div", { class: "delta" }, v.baseline
          ? tr(`aus ${v.baseline.sessions} Sessions`, `from ${v.baseline.sessions} sessions`)
          : tr("wird aufgebaut", "being built")))));
  }

  const regressions = game.sessions.filter((s) => s.verdict.status === "regression").reverse();
  if (regressions.length) {
    out.push(h("h2", {}, tr("Einbrüche", "Drops")), ...regressions.slice(0, 3).map((r) => regressionCard(game, r)));
  }

  if ((data.vault || []).some((v) => v.game_id === game.id)) {
    out.push(h("p", {}, h("button", { class: "btn small", onclick: () => show({ kind: "vault", id: game.id }) },
      tr("Gesicherte Einstellungen dieses Spiels", "Saved settings of this game"))));
  }
  out.push(h("h2", {}, tr("Verlauf", "History")), chart(game));

  out.push(h("h2", {}, tr("Sessions", "Sessions")), h("div", { class: "card" }, h("table", {},
    h("thead", {}, h("tr", {},
      h("th", {}, tr("Beginn", "Start")), h("th", {}, tr("Dauer", "Duration")), h("th", { class: "num" }, avgLabel()),
      h("th", { class: "num" }, lowLabel()), h("th", { class: "num" }, tr("Ruckler/min", "Stutters/min")), h("th", {}, tr("Bewertung", "Rating")),
      h("th", {}, tr("Hintergrundlast", "Background load")), h("th", {}, ""))),
    h("tbody", {}, [...game.sessions].reverse().map((r) => {
      const m = r.session.metrics;
      return h("tr", { class: r.verdict.status === "regression" ? "bad" : r.session.excluded ? "excluded" : "" },
        h("td", {}, fmtDateTime(r.session.started_at)),
        h("td", {}, fmtDuration(r.session)),
        h("td", { class: "num" }, m ? fmtFps(m.avg_fps) : "–"),
        h("td", { class: "num" }, m ? fmtFps(m.p1_low_fps) : "–"),
        h("td", { class: "num" }, m ? m.stutters_per_min.toFixed(1) : "–"),
        h("td", {}, m || r.session.excluded ? verdictBadge(r.verdict)
          : h("span", { class: "verdict muted" }, r.session.capture_note || tr("keine Frame-Daten", "no frame data"))),
        h("td", { class: "load" }, loadText(r)),
        h("td", { class: "num" }, m ? excludeButton(r) : null));
    })))));
  return out;
}

// ---- Loadout Vault ---------------------------------------------------------

const reasonText = (reason) => ({
  baseline: tr("Ausgangsstand", "Initial version"),
  changed: tr("Änderung", "Change"),
  before_restore: tr("Vor Wiederherstellung gesichert", "Backed up before restore"),
  restored: tr("Wiederhergestellt", "Restored"),
})[reason] || reason;

const classText = (c) => ({
  display: tr("Anzeige", "Display"),
  graphics: tr("Grafik", "Graphics"),
  personal: tr("Persönlich", "Personal"),
})[c] || c;

const SELECTIVE_EXT = ["ini", "cfg", "vcfg", "vdf", "txt", "lst"];
const supportsSelective = (name) => SELECTIVE_EXT.includes((name.split(".").pop() || "").toLowerCase());

function snapshotSummary(s) {
  if (s.reason !== "changed") return null;
  const sum = s.summary;
  let text = sum.binary ? tr("Datei geändert", "File changed")
    : tr(`${sum.changed} von ${sum.total} Einträgen geändert`, `${sum.changed} of ${sum.total} settings changed`);
  if (sum.build_changed) text += tr(" · nach Spiel-Update", " · after a game update");
  return text;
}

function vaultOverview() {
  const out = [h("h1", {}, "Loadout Vault"),
    h("p", { class: "sub" }, tr("Sichert die Einstellungsdateien deiner Spiele bei jeder Änderung. Setzt ein Patch Grafik oder Tastenbelegung zurück, holst du den alten Stand mit einem Klick zurück.",
      "Backs up the settings files of your games whenever they change. If a patch resets graphics or key bindings, you get the old version back with one click."))];
  if (!data.vault_enabled) {
    out.push(h("div", { class: "card" }, h("p", { class: "note" },
      tr("Loadout Vault ist in der Konfiguration deaktiviert (vault_enabled).", "Loadout Vault is disabled in the configuration (vault_enabled)."))));
    return out;
  }
  if (!data.vault.length) {
    out.push(h("div", { class: "card" }, h("p", { class: "note" },
      tr("Noch keine Einstellungsdateien gefunden. Erkannt werden Unreal-Engine-Spiele, Source-2-Spiele (CS2, Dota 2, Deadlock) und einige bekannte Titel. ",
        "No settings files found yet. Unreal Engine games, Source 2 games (CS2, Dota 2, Deadlock) and some well-known titles are detected. "),
      tr("Weitere Dateien lassen sich in config.json unter vault_custom_paths eintragen.", "More files can be added in config.json under vault_custom_paths."))));
    return out;
  }
  out.push(transferCard());
  out.push(h("div", { class: "vault-grid" }, data.vault.map((g) => {
    const versions = g.files.reduce((n, f) => n + f.snapshots.length, 0);
    return h("button", { class: `card vault-card${g.reset_suspected ? " alert" : ""}`, onclick: () => show({ kind: "vault", id: g.game_id }) },
      h("h3", {}, g.game_name),
      h("div", { class: "meta" }, tr(`${g.files.length} Datei(en) · ${versions} Stände`, `${g.files.length} file(s) · ${versions} versions`)),
      g.reset_suspected
        ? h("span", { class: "verdict bad" }, tr("Möglicher Reset erkannt", "Possible reset detected"))
        : h("span", { class: "verdict muted" }, g.last_change_at
          ? tr(`Zuletzt geändert ${fmtDateTime(g.last_change_at)}`, `Last changed ${fmtDateTime(g.last_change_at)}`)
          : tr("Unverändert seit Beginn der Sicherung", "Unchanged since backups began")));
  })));
  return out;
}

function diffTable(d) {
  if (d.binary) return h("p", { class: "note" }, tr("Binärdatei – Unterschiede können nicht einzeln angezeigt werden.", "Binary file – differences cannot be shown individually."));
  if (!d.changes.length) return h("p", { class: "note" }, tr("Keine inhaltlichen Unterschiede.", "No differences in content."));
  const rows = d.changes.slice(0, 300).map((c) => h("tr", {},
    h("td", {}, h("span", { class: `tag cls-${c.class}` }, classText(c.class))),
    h("td", { class: "mono" }, c.key),
    h("td", { class: "mono" }, c.old == null ? h("span", { class: "verdict muted" }, "—") : h("span", { class: "old" }, c.old)),
    h("td", { class: "mono" }, c.new == null ? h("span", { class: "verdict muted" }, tr("entfernt", "removed")) : c.new)));
  return h("div", {},
    h("p", { class: "note" }, d.previous_id
      ? tr(`${d.changes.length} Unterschied(e) zum vorherigen Stand:`, `${d.changes.length} difference(s) to the previous version:`)
      : tr(`Ausgangsstand mit ${d.changes.length} Einträgen:`, `Initial version with ${d.changes.length} settings:`)),
    h("table", { class: "diff" },
      h("thead", {}, h("tr", {}, h("th", {}, tr("Art", "Type")), h("th", {}, tr("Einstellung", "Setting")),
        h("th", {}, tr("Vorher", "Before")), h("th", {}, tr("Nachher", "After")))),
      h("tbody", {}, rows)),
    d.changes.length > 300 ? h("p", { class: "note" }, tr(`… und ${d.changes.length - 300} weitere.`, `… and ${d.changes.length - 300} more.`)) : null);
}

function snapshotRow(file, s, msg) {
  const panel = h("div", { class: "diff-panel" });
  const diffBtn = h("button", { class: "btn small" }, tr("Unterschiede", "Differences"));
  diffBtn.addEventListener("click", async () => {
    if (panel.childNodes.length) { panel.replaceChildren(); diffBtn.textContent = tr("Unterschiede", "Differences"); return; }
    diffBtn.disabled = true;
    try {
      panel.replaceChildren(diffTable(await backend.vaultDiff(s.id)));
      diffBtn.textContent = tr("Ausblenden", "Hide");
    } catch (e) {
      panel.replaceChildren(h("p", { class: "msg bad" }, String(e)));
    }
    diffBtn.disabled = false;
  });
  const when = fmtDateTime(s.taken_at);
  const restoreButton = (label, personalOnly) => {
    const btn = h("button", { class: "btn small" }, label);
    btn.addEventListener("click", async () => {
      const question = personalOnly
        ? tr(`Persönliche Einstellungen (Steuerung, Empfindlichkeit, Audio, Spiel) vom ${when} in „${file.label}“ übernehmen?\n\nAnzeige- und Grafikeinstellungen bleiben, wie sie jetzt sind. Der aktuelle Stand wird vorher gesichert.`,
          `Apply the personal settings (controls, sensitivity, audio, gameplay) from ${when} to "${file.label}"?\n\nDisplay and graphics settings stay as they are. The current version is backed up first.`)
        : tr(`Stand vom ${when} von „${file.label}“ vollständig wiederherstellen?\n\nDer aktuelle Stand wird vorher gesichert und lässt sich jederzeit zurückholen. Das Spiel sollte dabei geschlossen sein.`,
          `Fully restore the version of "${file.label}" from ${when}?\n\nThe current version is backed up first and can be restored at any time. The game should be closed.`);
      if (!window.confirm(question)) return;
      btn.disabled = true;
      try {
        const out = await backend.vaultRestore(s.id, personalOnly);
        msg.className = "msg good";
        msg.textContent = out && out.personal_keys != null
          ? tr(`${out.personal_keys} persönliche Einstellung(en) vom ${when} übernommen.`, `${out.personal_keys} personal setting(s) from ${when} applied.`)
          : tr(`Stand vom ${when} wiederhergestellt.`, `Version from ${when} restored.`);
        await refresh();
      } catch (e) {
        msg.className = "msg bad";
        msg.textContent = String(e);
        btn.disabled = false;
      }
    });
    return btn;
  };
  const restoreBtns = s.current ? [] : [
    supportsSelective(file.file_name) ? restoreButton(tr("Nur Steuerung & Spiel", "Controls & gameplay only"), true) : null,
    restoreButton(tr("Wiederherstellen", "Restore"), false),
  ];
  const summary = snapshotSummary(s);
  return h("div", { class: "snap" },
    h("div", { class: "snap-row" },
      h("span", { class: "time" }, when),
      h("span", {},
        h("span", { class: `tag${s.summary.reset_likely ? " tag-bad" : ""}` }, s.summary.reset_likely ? tr("Möglicher Reset", "Possible reset") : reasonText(s.reason)),
        summary ? h("span", { class: "snap-sum" }, summary) : null,
        s.current ? h("span", { class: "tag tag-good" }, tr("aktuell", "current")) : null),
      h("span", { class: "snap-actions" }, diffBtn, restoreBtns)),
    panel);
}

function transferCard() {
  const msg = h("div", { class: "msg" });
  const preview = h("div", {});
  const exportBtn = h("button", { class: "btn small" }, tr("Profil exportieren", "Export profile"));
  exportBtn.addEventListener("click", async () => {
    try {
      const res = await backend.vaultExport();
      if (!res) return;
      msg.className = "msg good";
      msg.textContent = tr(`${res.files} Datei(en) gespeichert: ${res.path}`, `${res.files} file(s) saved: ${res.path}`);
    } catch (e) { msg.className = "msg bad"; msg.textContent = String(e); }
  });
  const importBtn = h("button", { class: "btn small" }, tr("Profil importieren", "Import profile"));
  importBtn.addEventListener("click", async () => {
    preview.replaceChildren();
    msg.className = "msg"; msg.textContent = "";
    try {
      const path = await backend.vaultPickProfile();
      if (!path) return;
      const items = await backend.vaultImport(path, false);
      const pending = items.filter((i) => i.changed.length);
      const skipped = items.filter((i) => i.skipped);
      if (!items.length) { msg.textContent = tr("Keine passenden Spiele oder Dateien auf diesem PC gefunden.", "No matching games or files found on this PC."); return; }
      if (!pending.length) { msg.className = "msg good"; msg.textContent = tr("Alle persönlichen Einstellungen stimmen bereits überein.", "All personal settings already match."); return; }
      const apply = h("button", { class: "btn primary" }, tr(`${pending.length} Datei(en) übernehmen`, `Apply to ${pending.length} file(s)`));
      apply.addEventListener("click", async () => {
        apply.disabled = true;
        try {
          const done = await backend.vaultImport(path, true);
          preview.replaceChildren();
          const n = done.filter((i) => i.applied).length;
          msg.className = "msg good";
          msg.textContent = tr(`Übernommen in ${n} Datei(en). Die vorherigen Stände sind gesichert.`, `Applied to ${n} file(s). The previous versions are backed up.`);
          await refresh();
        } catch (e) { msg.className = "msg bad"; msg.textContent = String(e); apply.disabled = false; }
      });
      preview.replaceChildren(
        h("p", { class: "note" }, tr("Diese persönlichen Einstellungen würden übernommen. Anzeige und Grafik bleiben unverändert.",
          "These personal settings would be applied. Display and graphics stay unchanged.")),
        h("table", {}, h("tbody", {}, pending.map((i) => h("tr", {},
          h("td", {}, i.game_name), h("td", { class: "mono" }, i.label),
          h("td", { class: "num" }, tr(`${i.changed.length} Einstellung(en)`, `${i.changed.length} setting(s)`)))))),
        skipped.length ? h("p", { class: "note" }, tr(`${skipped.length} Datei(en) übersprungen: Auf diesem PC gibt es mehrere Steam-Konten, das Ziel ist nicht eindeutig.`,
          `${skipped.length} file(s) skipped: this PC has several Steam accounts, the target is ambiguous.`)) : null,
        h("div", { class: "field" }, apply));
    } catch (e) { msg.className = "msg bad"; msg.textContent = String(e); }
  });
  return h("div", { class: "card transfer" },
    h("h3", {}, tr("Neuer PC?", "New PC?")),
    h("p", { class: "note" }, tr("Exportiere deine Tastenbelegungen, Empfindlichkeit und Spieloptionen in eine Datei und importiere sie auf dem neuen PC. Auflösung und Grafikqualität werden dort nicht überschrieben, weil sie zur jeweiligen Hardware gehören.",
      "Export your key bindings, sensitivity and gameplay options to a file and import them on the new PC. Resolution and graphics quality are not overwritten there, because they belong to the hardware.")),
    h("div", { class: "field" }, exportBtn, importBtn), msg, preview);
}

function vaultGameView(g) {
  const out = [h("h1", {}, g.game_name),
    h("p", { class: "sub" }, tr("Jeder Stand ist eine vollständige Kopie der Datei. Wiederherstellen sichert den aktuellen Stand vorher, damit nichts verloren geht.",
      "Every version is a complete copy of the file. Restoring backs up the current version first, so nothing is lost."))];
  if (g.reset_suspected) {
    out.push(h("div", { class: "card alert" },
      h("h3", {}, tr("Möglicher Reset der Einstellungen", "Possible reset of the settings")),
      h("p", { class: "note" }, tr("Mindestens eine Datei hat sich auf einen Schlag stark verändert, oft direkt nach einem Spiel-Update. Prüfe die Unterschiede und stelle bei Bedarf den vorherigen Stand wieder her.",
        "At least one file changed a lot at once, often right after a game update. Check the differences and restore the previous version if needed."))));
  }
  if (data.games.some((x) => x.id === g.game_id)) {
    out.push(h("p", {}, h("button", { class: "btn small", onclick: () => show({ kind: "game", id: g.game_id }) },
      tr("Leistungsverlauf dieses Spiels ansehen", "View the performance history of this game"))));
  }
  for (const f of g.files) {
    const msg = h("div", { class: "msg" });
    out.push(h("h2", { class: "file" }, f.label),
      h("div", { class: "card" },
        h("div", { class: "mono path" }, f.path, f.exists ? null : h("span", { class: "tag tag-bad" }, tr("Datei fehlt", "File missing"))),
        msg,
        f.snapshots.map((s) => snapshotRow(f, s, msg))));
  }
  return out;
}

// ---- other views -----------------------------------------------------------

function changesView() {
  const out = [h("h1", {}, tr("Änderungsjournal", "Change journal")),
    h("p", { class: "sub" }, tr("Jede erkannte Änderung am System, neueste zuerst. Die Uhrzeit ist der Zeitpunkt des Scans, der sie bemerkt hat.",
      "Every detected change to the system, newest first. The time is when the scan noticed it."))];
  if (!data.changes.length) {
    out.push(h("div", { class: "card" }, h("p", { class: "note" },
      tr("Noch keine Änderungen erfasst. Der erste Scan legt nur den Ausgangszustand fest; ab dann wird jede Abweichung protokolliert.",
        "No changes recorded yet. The first scan only captures the starting point; every difference after that is logged."))));
    return out;
  }
  let day = null, card = null;
  for (const c of data.changes) {
    const d = fmtDay(c.detected_at);
    if (d !== day) {
      day = d;
      card = h("div", { class: "card" });
      out.push(h("div", { class: "day" }, d), card);
    }
    card.append(h("div", { class: "change-row" },
      h("span", { class: "time" }, fmtTime(c.detected_at)),
      h("span", {}, h("span", { class: "tag" }, catLabel(c.category))),
      h("span", {}, h("strong", {}, c.label), " ", changeDetail(c))));
  }
  return out;
}

function stateView() {
  const out = [h("h1", {}, tr("Systemzustand", "System state")),
    h("p", { class: "sub" }, tr("Was Blackbox beim letzten Scan erfasst hat. Abweichungen davon landen im Änderungsjournal.",
      "What Blackbox captured in the last scan. Differences to it go into the change journal."))];
  const groups = new Map();
  for (const f of data.state) {
    if (!groups.has(f.category)) groups.set(f.category, []);
    groups.get(f.category).push(f);
  }
  if (!groups.size) out.push(h("div", { class: "card" }, h("p", { class: "note" }, tr("Der erste Scan läuft noch.", "The first scan is still running."))));
  for (const [cat, facts] of groups) {
    out.push(h("h2", {}, `${catLabel(cat)} (${facts.length})`),
      h("div", { class: "card" }, h("div", { class: "kv" },
        facts.flatMap((f) => [h("div", { class: "k" }, f.label), h("div", { class: "mono" }, f.value)]))));
  }
  return out;
}

function rightsBlock() {
  const rights = data.capture_rights;
  if (rights === "elevated") {
    return h("p", { class: "msg good" }, tr("Blackbox läuft mit Administratorrechten – die Frame-Messung funktioniert.",
      "Blackbox runs with administrator rights – frame capture works."));
  }
  if (rights === "performance_log_users") {
    return h("p", { class: "msg good" }, tr("Frame-Messung ohne Administratorrechte ist eingerichtet.", "Frame capture without administrator rights is set up."));
  }
  const msg = h("div", { class: "msg" });
  const btn = h("button", { class: "btn" }, tr("Frame-Messung dauerhaft erlauben", "Allow frame capture permanently"));
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    msg.className = "msg";
    msg.textContent = tr("Bitte die Windows-Abfrage bestätigen …", "Please confirm the Windows prompt …");
    try {
      await backend.requestCaptureRights();
      msg.className = "msg good";
      msg.textContent = tr("Erledigt. Wirksam nach der nächsten Windows-Anmeldung (abmelden und wieder anmelden).",
        "Done. Takes effect after the next Windows sign-in (sign out and back in).");
    } catch (e) {
      msg.className = "msg bad";
      msg.textContent = String(e);
      btn.disabled = false;
    }
  });
  return h("div", { class: "rights" },
    h("p", { class: "note" },
      tr("PresentMon braucht Zugriff auf die Windows-Leistungsereignisse. Dafür gibt es zwei Wege: Blackbox jedes Mal als Administrator starten, ",
        "PresentMon needs access to the Windows performance events. There are two ways: start Blackbox as administrator every time, "),
      tr("oder dein Windows-Konto einmalig in die Gruppe „Leistungsprotokollbenutzer“ aufnehmen. Danach misst Blackbox auch beim normalen Start und im Autostart.",
        "or add your Windows account once to the group \"Performance Log Users\". After that Blackbox also measures when started normally and at sign-in.")),
    h("div", { class: "field" }, btn), msg);
}

function autostartBlock() {
  const box = h("input", { type: "checkbox", id: "autostart" });
  box.checked = !!data.autostart;
  const msg = h("div", { class: "msg" });
  box.addEventListener("change", async () => {
    box.disabled = true;
    try {
      await backend.setAutostart(box.checked);
      msg.className = "msg good";
      msg.textContent = box.checked
        ? tr("Blackbox startet ab jetzt mit Windows, unauffällig im Infobereich.", "Blackbox now starts with Windows, quietly in the notification area.")
        : tr("Autostart deaktiviert.", "Autostart disabled.");
      await refresh(false);
    } catch (e) {
      msg.className = "msg bad";
      msg.textContent = String(e);
      box.checked = !box.checked;
    }
    box.disabled = false;
  });
  return h("div", { class: "card" },
    h("label", { class: "check", for: "autostart" }, box,
      tr(" Blackbox beim Anmelden automatisch starten (im Infobereich, ohne Fenster)", " Start Blackbox automatically at sign-in (in the notification area, no window)")),
    msg,
    h("p", { class: "note" }, tr("Nur mit Autostart entgeht Blackbox keine Session und kein Patch. Der Eintrag liegt in deinem Benutzerkonto und braucht keine Administratorrechte.",
      "Only with autostart does Blackbox catch every session and patch. The entry lives in your user account and needs no administrator rights.")));
}

function notificationsBlock() {
  const box = h("input", { type: "checkbox", id: "notifications" });
  box.checked = data.notifications !== false;
  box.addEventListener("change", async () => {
    box.disabled = true;
    try { await backend.setNotifications(box.checked); await refresh(false); }
    catch (e) { box.checked = !box.checked; window.alert(String(e)); }
    box.disabled = false;
  });
  return h("div", { class: "card" },
    h("label", { class: "check", for: "notifications" }, box,
      tr(" Windows-Benachrichtigung bei Leistungseinbruch oder möglichem Reset der Spieleinstellungen",
        " Windows notification on a performance drop or a possible reset of game settings")),
    h("p", { class: "note" }, tr("Blackbox meldet sich nur bei diesen beiden Ereignissen, sonst nie.", "Blackbox only speaks up for these two events, never otherwise.")));
}

function languageBlock() {
  const select = h("select", { id: "language" },
    h("option", { value: "auto" }, tr("Automatisch (Windows-Sprache)", "Automatic (Windows language)")),
    h("option", { value: "de" }, "Deutsch"),
    h("option", { value: "en" }, "English"));
  select.value = data.language_setting || "auto";
  select.addEventListener("change", async () => {
    select.disabled = true;
    try { await backend.setLanguage(select.value); await refresh(); }
    catch (e) { window.alert(String(e)); }
    select.disabled = false;
  });
  return h("div", { class: "card" },
    h("div", { class: "field" }, select),
    h("p", { class: "note" }, tr("Gilt für die App, Benachrichtigungen, Berichte und das Menü im Infobereich.",
      "Applies to the app, notifications, reports and the notification area menu.")));
}

function settingsView() {
  const input = h("input", {
    type: "text", spellcheck: "false",
    placeholder: "C:\\Tools\\PresentMon-2.6.0-x64.exe",
    value: data.presentmon_path || "",
  });
  const msg = h("div", { class: "msg" });
  const save = h("button", { class: "btn primary" }, tr("Speichern", "Save"));
  save.addEventListener("click", async () => {
    save.disabled = true;
    try {
      await backend.setPresentmon(input.value);
      msg.className = "msg good";
      msg.textContent = input.value.trim()
        ? tr("Gespeichert. Gilt ab der nächsten Session.", "Saved. Applies from the next session.")
        : data.presentmon_bundled
          ? tr("Eigener Pfad entfernt – das mitgelieferte PresentMon wird verwendet.", "Own path removed – the bundled PresentMon is used.")
          : tr("Frame-Messung deaktiviert.", "Frame capture disabled.");
      await refresh(false);
    } catch (e) {
      msg.className = "msg bad";
      msg.textContent = String(e);
    }
    save.disabled = false;
  });
  return [
    h("h1", {}, tr("Einstellungen", "Settings")),
    h("h2", {}, tr("Sprache", "Language")),
    languageBlock(),
    h("h2", {}, tr("Frame-Messung", "Frame capture")),
    h("div", { class: "card" },
      h("p", { class: "note" },
        tr("Blackbox misst die Bildrate mit PresentMon von Intel. Das Programm liest nur Windows-Ereignisse mit und greift nicht in Spiele ein.",
          "Blackbox measures the frame rate with PresentMon by Intel. It only reads Windows events and does not touch games.")),
      data.presentmon_bundled
        ? h("p", { class: "msg good" }, tr("PresentMon ist mitinstalliert und wird automatisch verwendet.", "PresentMon is installed with Blackbox and used automatically."))
        : h("p", { class: "note" }, tr("Kein mitgeliefertes PresentMon gefunden (z. B. bei einem Entwickler-Build). Pfad unten eintragen; Download: github.com/GameTechDev/PresentMon/releases.",
          "No bundled PresentMon found (e.g. in a developer build). Enter its path below; download: github.com/GameTechDev/PresentMon/releases.")),
      h("p", { class: "note" }, data.presentmon_bundled
        ? tr("Nur falls du eine andere PresentMon-Version nutzen willst, trage hier ihren Pfad ein. Leer lassen = mitgeliefertes PresentMon.",
          "Only if you want to use another PresentMon version, enter its path here. Leave empty = bundled PresentMon.")
        : tr("Pfad zu PresentMon:", "Path to PresentMon:")),
      h("div", { class: "field" }, input, save), msg,
      rightsBlock()),
    h("h2", {}, tr("Start mit Windows", "Start with Windows")),
    autostartBlock(),
    h("h2", {}, tr("Benachrichtigungen", "Notifications")),
    notificationsBlock(),
    h("h2", {}, tr("Speicherorte", "Storage")),
    h("div", { class: "card" }, h("div", { class: "kv" },
      h("div", { class: "k" }, tr("Datenbank", "Database")), h("div", { class: "mono" }, data.database),
      h("div", { class: "k" }, tr("Protokolle", "Logs")), h("div", { class: "mono" }, data.log_dir))),
    h("p", { class: "note" }, tr("Alle Daten bleiben auf diesem PC. Blackbox baut keine Netzwerkverbindungen auf.",
      "All data stays on this PC. Blackbox makes no network connections.")),
  ];
}

function welcomeView() {
  return [h("div", { class: "empty" },
    h("h1", {}, tr("Blackbox zeichnet auf", "Blackbox is recording")),
    h("p", {}, tr("Der Ausgangszustand deines Systems ist erfasst. Sobald du ein Spiel startest, beginnt die erste Session.",
      "The starting point of your system is captured. As soon as you start a game, the first session begins.")),
    h("ol", {},
      h("li", {}, tr("Spiele wie gewohnt. Blackbox läuft im Infobereich weiter.", "Play as usual. Blackbox keeps running in the notification area.")),
      h("li", {}, tr("Nach drei Sessions eines Spiels steht die Vergleichsbasis.", "After three sessions of a game, the baseline is ready.")),
      h("li", {}, tr("Bricht die Leistung ein, siehst du hier, was sich seitdem geändert hat.", "If performance drops, you see here what has changed since."))),
    (data.vault || []).length
      ? h("p", {}, tr(`Die Einstellungen von ${data.vault.length} Spiel(en) werden bereits gesichert. `,
        `The settings of ${data.vault.length} game(s) are already backed up. `),
      h("button", { class: "btn small", onclick: () => show({ kind: "vault" }) }, tr("Loadout Vault öffnen", "Open Loadout Vault")))
      : null)];
}

// ---- wiring ----------------------------------------------------------------

function render() {
  if (view.kind === "auto") {
    const g = data.games.find((g) => g.regressed) || data.games[0];
    view = g ? { kind: "game", id: g.id } : { kind: "welcome" };
  }
  if (view.kind === "welcome" && data.games.length) view = { kind: "game", id: data.games[0].id };
  let content;
  if (view.kind === "game") {
    const game = data.games.find((g) => g.id === view.id);
    content = game ? gameView(game) : welcomeView();
  } else if (view.kind === "vault") {
    const g = view.id && data.vault.find((x) => x.game_id === view.id);
    content = g ? vaultGameView(g) : vaultOverview();
  } else if (view.kind === "changes") content = changesView();
  else if (view.kind === "state") content = stateView();
  else if (view.kind === "settings") content = settingsView();
  else content = welcomeView();
  renderStatus();
  renderSidebar();
  document.getElementById("main").replaceChildren(...content);
}

function show(next) {
  view = next;
  render();
  document.getElementById("main").scrollTop = 0;
}

async function refresh(rerender = true) {
  try {
    data = await backend.dashboard();
  } catch (e) {
    document.getElementById("main").replaceChildren(h("div", { class: "empty" },
      h("h1", {}, tr("Daten konnten nicht geladen werden", "Data could not be loaded")), h("p", {}, String(e))));
    return;
  }
  // Do not re-render the settings form while the user is typing in it.
  const busy = (view.kind === "settings" && document.activeElement?.tagName === "INPUT") ||
    (view.kind === "vault" && document.querySelector(".diff-panel table, .transfer table"));
  if (rerender && busy) {
    renderStatus();
    renderSidebar();
    return;
  }
  if (rerender) render(); else { renderStatus(); renderSidebar(); }
}

document.getElementById("scan").addEventListener("click", async (e) => {
  e.target.disabled = true;
  await backend.scanNow();
  setTimeout(() => { e.target.disabled = false; }, 1500);
});
for (const btn of document.querySelectorAll(".nav-item[data-view]")) {
  btn.addEventListener("click", () => show({ kind: btn.dataset.view }));
}

backend.onUpdate(() => refresh());
setInterval(refresh, 60_000);
refresh();
