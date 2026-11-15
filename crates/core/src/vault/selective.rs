//! Separating personal settings from hardware-dependent ones.
//!
//! Display (resolution, refresh rate, window mode) and graphics settings
//! (quality presets, upscaling, frame limits) belong to a PC. Key bindings,
//! sensitivity, audio and gameplay options belong to the player. Selective
//! restores and profile imports move only the personal part and keep the
//! hardware part of the target file untouched.
//!
//! Merging rewrites values line by line, so comments, ordering and
//! formatting of the target file survive. Supported: INI, Valve KeyValues
//! (`.vcfg`, `video.txt`, `.lst`) and Source `.cfg`.

use std::collections::HashMap;

use serde::Serialize;

use super::parse::{self, Content, Entries};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyClass {
    /// Resolution, refresh rate, window mode, monitor.
    Display,
    /// Quality settings that depend on the GPU.
    Graphics,
    /// Controls, sensitivity, audio, gameplay, interface.
    Personal,
}

impl KeyClass {
    pub fn is_hardware(self) -> bool {
        matches!(self, KeyClass::Display | KeyClass::Graphics)
    }
}

const PERSONAL_MARKERS: &[&str] = &[
    "bind", "key", "sensitivity", "mouse", "invert", "volume", "audio", "sound", "snd_", "voice", "music",
    "crosshair", "language", "subtitle", "hud", "fov", "gameplay", "control", "gamepad", "controller",
];

const DISPLAY_MARKERS: &[&str] = &[
    "resolution", "defaultres", "screenwidth", "screenheight", "refresh", "fullscreen", "windowmode",
    "window_mode", "displaymode", "monitor", "adapter", "windowpos", "lastconfirmed", "desiredscreen",
    "aspectratio", "aspect_ratio", "hdr",
];

const GRAPHICS_MARKERS: &[&str] = &[
    "quality", "shadow", "texture", "aniso", "antialias", "msaa", "fxaa", "taa", "dlss", "fsr", "xess",
    "upscal", "sharpen", "vsync", "framerate", "fps", "lod", "viewdistance", "foliage", "effects",
    "postprocess", "reflection", "ssao", "ambientocclusion", "bloom", "motionblur", "raytrac", "dof",
    "depthoffield", "screenpercentage", "shader", "tessellation", "volumetric", "detail", "preset",
    "nvidia_reflex", "low_latency", "graphics",
];

const GRAPHICS_PREFIXES: &[&str] = &["r.", "sg.", "r_", "mat_", "gpu_"];

/// Classify a flattened setting key (as produced by `parse`).
pub fn classify(key: &str) -> KeyClass {
    let lower = key.to_lowercase();
    // The setting name is the last path segment, without a "[Section] " or
    // "setting." prefix and without a duplicate counter.
    let name = lower.rsplit(['/', ']']).next().unwrap_or(&lower).trim();
    let name = name.split(" #").next().unwrap_or(name);
    let name = name.strip_prefix("setting.").unwrap_or(name);
    let name = name.strip_prefix("convars/").unwrap_or(name);

    if PERSONAL_MARKERS.iter().any(|m| name.contains(m)) {
        return KeyClass::Personal;
    }
    if DISPLAY_MARKERS.iter().any(|m| name.contains(m)) {
        return KeyClass::Display;
    }
    if GRAPHICS_PREFIXES.iter().any(|p| name.starts_with(p)) || GRAPHICS_MARKERS.iter().any(|m| name.contains(m)) {
        return KeyClass::Graphics;
    }
    KeyClass::Personal
}

fn ext(file_name: &str) -> String {
    file_name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Whether `merge_values` can rewrite this kind of file.
pub fn supports_selective(file_name: &str) -> bool {
    matches!(ext(file_name).as_str(), "ini" | "cfg" | "vcfg" | "vdf" | "txt" | "lst")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Ini,
    KeyValues,
    Cfg,
}

fn detect_format(text: &str, file_name: &str) -> Option<Format> {
    match ext(file_name).as_str() {
        "ini" => Some(Format::Ini),
        "cfg" => Some(Format::Cfg),
        "vcfg" | "vdf" => Some(Format::KeyValues),
        "txt" | "lst" => Some(if parse::parse_kv(text).is_some() { Format::KeyValues } else { Format::Cfg }),
        _ => None,
    }
}

/// One line that carries a setting: its key and how to rebuild the line
/// with another value.
struct ValueLine {
    key: String,
    /// Everything up to the value.
    prefix: String,
    /// Everything after the value (closing quote, comments).
    suffix: String,
}

fn ini_lines(text: &str) -> Vec<Option<ValueLine>> {
    let mut section = String::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    text.split('\n')
        .map(|raw| {
            let line = raw.trim_end_matches('\r');
            let t = line.trim();
            if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
                return None;
            }
            if t.starts_with('[') && t.ends_with(']') {
                section = t[1..t.len() - 1].trim().to_string();
                return None;
            }
            let eq = line.find('=')?;
            let k = line[..eq].trim();
            let base = if section.is_empty() { k.to_string() } else { format!("[{section}] {k}") };
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            let key = if *n == 1 { base } else { format!("{base} #{n}") };
            let after = &line[eq + 1..];
            let lead = after.len() - after.trim_start().len();
            Some(ValueLine {
                key,
                prefix: line[..eq + 1 + lead].to_string(),
                suffix: if raw.ends_with('\r') { "\r".into() } else { String::new() },
            })
        })
        .collect()
}

/// Positions of the quoted tokens of a line: (start of content, end of content).
fn quoted_spans(line: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        if c == '"' {
            match start {
                None => start = Some(i + 1),
                Some(s) => {
                    spans.push((s, i));
                    start = None;
                }
            }
        } else if c == '/' && start.is_none() && line[i..].starts_with("//") {
            break;
        }
    }
    spans
}

fn kv_lines(text: &str) -> Vec<Option<ValueLine>> {
    let mut stack: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    text.split('\n')
        .map(|raw| {
            let line = raw.trim_end_matches('\r');
            let t = line.trim();
            let spans = quoted_spans(line);
            let opens = t.ends_with('{');
            if t == "{" {
                if let Some(name) = pending.take() {
                    stack.push(name);
                }
                return None;
            }
            if t.starts_with('}') {
                stack.pop();
                return None;
            }
            match spans.len() {
                1 => {
                    let name = line[spans[0].0..spans[0].1].to_lowercase();
                    if opens { stack.push(name) } else { pending = Some(name) }
                    None
                }
                n if n >= 2 && !line.contains('\\') => {
                    let k = line[spans[0].0..spans[0].1].to_lowercase();
                    let mut path = stack.clone();
                    path.push(k);
                    Some(ValueLine {
                        key: path.join("/"),
                        prefix: line[..spans[1].0].to_string(),
                        suffix: format!("{}{}", &line[spans[1].1..], if raw.ends_with('\r') { "\r" } else { "" }),
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn cfg_lines(text: &str) -> Vec<Option<ValueLine>> {
    text.split('\n')
        .map(|raw| {
            let line = raw.trim_end_matches('\r');
            let entries = parse::parse_cfg(line);
            let (key, _) = entries.into_iter().next()?;
            let indent = &line[..line.len() - line.trim_start().len()];
            let prefix = match key.strip_prefix("bind ") {
                Some(k) => format!("{indent}bind \"{k}\" \""),
                None => format!("{indent}{key} \""),
            };
            Some(ValueLine { key, prefix, suffix: format!("\"{}", if raw.ends_with('\r') { "\r" } else { "" }) })
        })
        .collect()
}

/// Look up a line key in flattened entries. KeyValues files drop their root
/// block name when flattened, so both spellings are tried.
fn lookup<'a>(entries: &'a Entries, key: &str, format: Format) -> Option<(&'a str, String)> {
    if let Some(v) = entries.get(key) {
        return Some((v, key.to_string()));
    }
    if format == Format::KeyValues {
        if let Some((_, rest)) = key.split_once('/') {
            if let Some(v) = entries.get(rest) {
                return Some((v, rest.to_string()));
            }
        }
    }
    None
}

/// Result of a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub bytes: Vec<u8>,
    /// Keys whose value was taken over.
    pub changed: Vec<String>,
}

/// Return `base` with the values of all keys for which `take` is true
/// replaced by their values in `donor`. Keys that exist only in `donor` are
/// not added. `None` if the format is unsupported or not decodable.
pub fn merge_values(base: &[u8], donor: &[u8], file_name: &str, take: impl Fn(&str) -> bool) -> Option<Merged> {
    let (text, encoding) = parse::decode_with_encoding(base)?;
    let Content::Text(donor_entries) = parse::parse(donor, file_name) else { return None };
    let format = detect_format(&text, file_name)?;
    let lines = match format {
        Format::Ini => ini_lines(&text),
        Format::KeyValues => kv_lines(&text),
        Format::Cfg => cfg_lines(&text),
    };
    let raw_lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::with_capacity(raw_lines.len());
    let mut changed = Vec::new();
    for (raw, info) in raw_lines.iter().zip(lines) {
        let replaced = info.and_then(|l| {
            let (value, key) = lookup(&donor_entries, &l.key, format)?;
            if !take(&key) {
                return None;
            }
            let rebuilt = format!("{}{}{}", l.prefix, value, l.suffix);
            (rebuilt != *raw).then(|| {
                changed.push(key);
                rebuilt
            })
        });
        out.push(replaced.unwrap_or_else(|| raw.to_string()));
    }
    Some(Merged { bytes: parse::encode(&out.join("\n"), encoding), changed })
}

/// Take over only the personal settings of `donor`.
pub fn merge_personal(base: &[u8], donor: &[u8], file_name: &str) -> Option<Merged> {
    merge_values(base, donor, file_name, |k| !classify(k).is_hardware())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let cases = [
            ("[/Script/Engine.GameUserSettings] ResolutionSizeX", KeyClass::Display),
            ("[/Script/Engine.GameUserSettings] FullscreenMode", KeyClass::Display),
            ("setting.defaultres", KeyClass::Display),
            ("setting.refreshrate_numerator", KeyClass::Display),
            ("[ScalabilityGroups] sg.ShadowQuality", KeyClass::Graphics),
            ("setting.r_texturefilteringquality", KeyClass::Graphics),
            ("setting.mat_vsync", KeyClass::Graphics),
            ("[SystemSettings] r.ScreenPercentage", KeyClass::Graphics),
            ("convars/fps_max", KeyClass::Graphics),
            ("bind w", KeyClass::Personal),
            ("bindings/mouse1", KeyClass::Personal),
            ("convars/sensitivity", KeyClass::Personal),
            ("convars/volume", KeyClass::Personal),
            ("convars/cl_crosshairsize", KeyClass::Personal),
            ("[/Script/Game.Settings] MouseSensitivity", KeyClass::Personal),
            ("[/Script/Game.Settings] bShowDamageNumbers", KeyClass::Personal),
        ];
        for (key, expected) in cases {
            assert_eq!(classify(key), expected, "{key}");
        }
    }

    #[test]
    fn ini_merge_keeps_hardware_and_formatting() {
        let current = "; my comment\r\n[Display]\r\nResolutionSizeX=3840\r\nsg.ShadowQuality = 3\r\n[Input]\r\nMouseSensitivity=0.5\r\n+Bind=A\r\n+Bind=B\r\n";
        let old = "[Display]\nResolutionSizeX=1920\nsg.ShadowQuality=1\n[Input]\nMouseSensitivity=1.25\n+Bind=A\n+Bind=X\nOnlyInOld=1\n";
        let m = merge_personal(current.as_bytes(), old.as_bytes(), "GameUserSettings.ini").unwrap();
        let text = String::from_utf8(m.bytes).unwrap();
        assert_eq!(
            text,
            "; my comment\r\n[Display]\r\nResolutionSizeX=3840\r\nsg.ShadowQuality = 3\r\n[Input]\r\nMouseSensitivity=1.25\r\n+Bind=A\r\n+Bind=X\r\n"
        );
        assert_eq!(m.changed, vec!["[Input] MouseSensitivity", "[Input] +Bind #2"]);
    }

    #[test]
    fn keyvalues_merge_handles_nesting_and_root() {
        let current = "\"config\"\n{\n\t\"bindings\"\n\t{\n\t\t\"w\"\t\t\"+forward\"\n\t}\n\t\"convars\"\n\t{\n\t\t\"sensitivity\"\t\t\"1.0\"\n\t\t\"fps_max\"\t\t\"400\"\n\t}\n}\n";
        let old = "\"config\"\n{\n\t\"bindings\" { \"w\" \"+jump\" }\n\t\"convars\" { \"sensitivity\" \"2.2\" \"fps_max\" \"60\" }\n}\n";
        let m = merge_personal(current.as_bytes(), old.as_bytes(), "cs2_user_convars_0_slot0.vcfg").unwrap();
        let text = String::from_utf8(m.bytes).unwrap();
        assert!(text.contains("\t\t\"w\"\t\t\"+jump\""));
        assert!(text.contains("\t\t\"sensitivity\"\t\t\"2.2\""));
        assert!(text.contains("\t\t\"fps_max\"\t\t\"400\""), "graphics stays");
        assert_eq!(m.changed.len(), 2);
    }

    #[test]
    fn cfg_merge() {
        let current = "bind \"w\" \"+forward\"\nsensitivity \"1\"\nfps_max \"300\"\n";
        let old = "bind w +jump\nsensitivity 3\nfps_max 60\n";
        let m = merge_personal(current.as_bytes(), old.as_bytes(), "autoexec.cfg").unwrap();
        assert_eq!(String::from_utf8(m.bytes).unwrap(), "bind \"w\" \"+jump\"\nsensitivity \"3\"\nfps_max \"300\"\n");
    }

    #[test]
    fn utf16_files_stay_utf16() {
        let enc = |s: &str| {
            let mut b = vec![0xFF, 0xFE];
            b.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
            b
        };
        let current = enc("[A]\r\nMouseSensitivity=1\r\nResolutionSizeX=3840\r\n");
        let old = "[A]\nMouseSensitivity=2\nResolutionSizeX=1920\n";
        let m = merge_personal(&current, old.as_bytes(), "Input.ini").unwrap();
        assert_eq!(m.bytes, enc("[A]\r\nMouseSensitivity=2\r\nResolutionSizeX=3840\r\n"));
    }

    #[test]
    fn unsupported_formats() {
        assert!(!supports_selective("settings.json"));
        assert!(merge_personal(b"{}", b"{}", "settings.json").is_none());
        assert!(merge_personal(&[0, 1, 2], b"a=1", "x.ini").is_none());
    }
}
