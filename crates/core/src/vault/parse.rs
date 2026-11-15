//! Turns game config files into flat `key -> value` maps so two versions can
//! be compared setting by setting instead of byte by byte.

use std::collections::BTreeMap;

use serde::Serialize;

use super::selective::classify;
use crate::vdf;

pub type Entries = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyChange {
    pub key: String,
    /// `None` = the setting did not exist before.
    pub old: Option<String>,
    /// `None` = the setting no longer exists.
    pub new: Option<String>,
    /// Display, graphics or personal setting.
    pub class: super::selective::KeyClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(Entries),
    /// Not decodable as text; only "changed or not" can be said.
    Binary,
}

/// Text encoding of a config file, kept when a file is rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

/// Decode and remember the encoding.
pub fn decode_with_encoding(bytes: &[u8]) -> Option<(String, Encoding)> {
    let encoding = if bytes.starts_with(&[0xFF, 0xFE]) {
        Encoding::Utf16Le
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Encoding::Utf16Be
    } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Encoding::Utf8Bom
    } else {
        Encoding::Utf8
    };
    decode(bytes).map(|t| (t, encoding))
}

pub fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        Encoding::Utf16Le => [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        Encoding::Utf16Be => [0xFE, 0xFF].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect(),
    }
}

/// Decode UTF-8 (with or without BOM) and UTF-16 (with BOM).
pub fn decode(bytes: &[u8]) -> Option<String> {
    fn utf16(bytes: &[u8], le: bool) -> Option<String> {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) })
            .collect();
        String::from_utf16(&units).ok()
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0')).then(|| text.to_string())
}

fn insert_unique(map: &mut Entries, key: String, value: String) {
    // Repeated keys (e.g. Unreal "+Key=" arrays) get a running index.
    let mut candidate = key.clone();
    let mut n = 2;
    while map.contains_key(&candidate) {
        candidate = format!("{key} #{n}");
        n += 1;
    }
    map.insert(candidate, value);
}

pub fn parse_ini(text: &str) -> Entries {
    let mut map = Entries::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (line, ""),
        };
        let key = if section.is_empty() { k.to_string() } else { format!("[{section}] {k}") };
        insert_unique(&mut map, key, v.to_string());
    }
    map
}

fn tokenize_cfg_line(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut in_quotes = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_quotes {
            if c == '"' {
                in_quotes = false;
                tokens.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            if in_token {
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
            in_quotes = true;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            break; // comment outside quotes
        } else if c.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
        } else {
            cur.push(c);
            in_token = true;
        }
        i += 1;
    }
    if in_token || in_quotes {
        tokens.push(cur);
    }
    tokens
}

/// Source-engine style `.cfg`: `bind "w" "+forward"`, `sensitivity "1.2"`.
/// A later line overrides an earlier one, as in the game.
pub fn parse_cfg(text: &str) -> Entries {
    let mut map = Entries::new();
    for line in text.lines() {
        let tokens = tokenize_cfg_line(line);
        let Some(first) = tokens.first() else { continue };
        if first.eq_ignore_ascii_case("bind") && tokens.len() >= 2 {
            map.insert(format!("bind {}", tokens[1].to_lowercase()), tokens[2..].join(" "));
        } else {
            map.insert(first.clone(), tokens[1..].join(" "));
        }
    }
    map
}

fn flatten_kv(prefix: &str, block: &vdf::Block, out: &mut Entries) {
    for (k, v) in block {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}/{k}") };
        match v {
            vdf::Value::Str(s) => {
                out.insert(key, s.clone());
            }
            vdf::Value::Block(b) => flatten_kv(&key, b, out),
        }
    }
}

/// Valve KeyValues (`.vcfg`, `video.txt`). The root block name is dropped.
pub fn parse_kv(text: &str) -> Option<Entries> {
    let root = vdf::parse(text).ok()?;
    let mut out = Entries::new();
    if root.len() == 1 {
        if let Some(vdf::Value::Block(inner)) = root.values().next() {
            flatten_kv("", inner, &mut out);
            return Some(out);
        }
    }
    flatten_kv("", &root, &mut out);
    (!out.is_empty()).then_some(out)
}

fn flatten_json(prefix: &str, v: &serde_json::Value, out: &mut Entries) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}/{k}") };
                flatten_json(&key, v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                flatten_json(&format!("{prefix}[{i}]"), v, out);
            }
        }
        serde_json::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

pub fn parse_json(text: &str) -> Option<Entries> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut out = Entries::new();
    flatten_json("", &value, &mut out);
    Some(out)
}

/// Line based fallback with two XML shortcuts:
/// `<Shadow>HIGH</Shadow>` -> `Shadow = HIGH` and
/// `<shadowQuality value="2" />` -> `<shadowQuality /> = 2`.
pub fn parse_lines(text: &str) -> Entries {
    let mut map = Entries::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = xml_element_text(line).or_else(|| xml_value_attr(line)) {
            insert_unique(&mut map, key, value);
        } else {
            insert_unique(&mut map, line.to_string(), String::new());
        }
    }
    map
}

fn xml_element_text(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix('<')?;
    let name_end = rest.find(|c: char| c == '>' || c.is_whitespace())?;
    let name = &rest[..name_end];
    if name.is_empty() || name.starts_with(['/', '?', '!']) {
        return None;
    }
    let open_end = rest.find('>')?;
    let inner_and_close = &rest[open_end + 1..];
    let close = format!("</{name}>");
    let inner = inner_and_close.strip_suffix(close.as_str())?;
    if inner.contains('<') {
        return None;
    }
    Some((name.to_string(), inner.trim().to_string()))
}

fn xml_value_attr(line: &str) -> Option<(String, String)> {
    if !line.starts_with('<') {
        return None;
    }
    let start = line.find(" value=\"")?;
    let value_start = start + " value=\"".len();
    let len = line[value_start..].find('"')?;
    let value = &line[value_start..value_start + len];
    let key = format!("{}{}", &line[..start], &line[value_start + len + 1..]);
    Some((key, value.to_string()))
}

/// Parse a file; the format is chosen by extension with sensible fallbacks.
pub fn parse(bytes: &[u8], file_name: &str) -> Content {
    let Some(text) = decode(bytes) else {
        return Content::Binary;
    };
    let ext = file_name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let entries = match ext.as_str() {
        "ini" => parse_ini(&text),
        "json" => parse_json(&text).unwrap_or_else(|| parse_lines(&text)),
        "cfg" => parse_cfg(&text),
        "vcfg" | "vdf" | "txt" | "lst" => parse_kv(&text).unwrap_or_else(|| {
            if ext == "txt" || ext == "lst" { parse_cfg(&text) } else { parse_lines(&text) }
        }),
        _ => parse_lines(&text),
    };
    Content::Text(entries)
}

/// Setting-level differences, sorted by key.
pub fn diff(old: &Entries, new: &Entries) -> Vec<KeyChange> {
    let mut out = Vec::new();
    for (k, v) in old {
        match new.get(k) {
            None => out.push(KeyChange { key: k.clone(), old: Some(v.clone()), new: None, class: classify(k) }),
            Some(nv) if nv != v => out.push(KeyChange {
                key: k.clone(),
                old: Some(v.clone()),
                new: Some(nv.clone()),
                class: classify(k),
            }),
            Some(_) => {}
        }
    }
    for (k, v) in new {
        if !old.contains_key(k) {
            out.push(KeyChange { key: k.clone(), old: None, new: Some(v.clone()), class: classify(k) });
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Number of distinct keys in both versions together.
pub fn union_len(old: &Entries, new: &Entries) -> usize {
    old.len() + new.keys().filter(|k| !old.contains_key(*k)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8], name: &str) -> Entries {
        match parse(bytes, name) {
            Content::Text(e) => e,
            Content::Binary => panic!("unexpected binary"),
        }
    }

    #[test]
    fn unreal_ini_with_sections_and_arrays() {
        let ini = "[/Script/Engine.GameUserSettings]\nbUseVSync=False\nResolutionSizeX=2560\n; comment\n\n[ScalabilityGroups]\nsg.ShadowQuality=3\n+Paths=a\n+Paths=b\n";
        let e = text(ini.as_bytes(), "GameUserSettings.ini");
        assert_eq!(e["[/Script/Engine.GameUserSettings] bUseVSync"], "False");
        assert_eq!(e["[ScalabilityGroups] sg.ShadowQuality"], "3");
        assert_eq!(e["[ScalabilityGroups] +Paths"], "a");
        assert_eq!(e["[ScalabilityGroups] +Paths #2"], "b");
        assert_eq!(e.len(), 5);
    }

    #[test]
    fn source_cfg_binds_and_comments() {
        let cfg = "bind \"W\" \"+forward\"\nsensitivity \"1.5\" // mouse\n// only comment\nbind w \"+jump\"\nname \"a // b\"\n";
        let e = text(cfg.as_bytes(), "autoexec.cfg");
        assert_eq!(e["bind w"], "+jump");
        assert_eq!(e["sensitivity"], "1.5");
        assert_eq!(e["name"], "a // b");
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn keyvalues_vcfg_and_video_txt() {
        let vcfg = "\"config\"\n{\n\t\"bindings\"\n\t{\n\t\t\"w\" \"+forward\"\n\t}\n\t\"convars\" { \"sensitivity\" \"1.2\" }\n}";
        let e = text(vcfg.as_bytes(), "cs2_user_keys_0_slot0.vcfg");
        assert_eq!(e["bindings/w"], "+forward");
        assert_eq!(e["convars/sensitivity"], "1.2");

        let video = "\"video.cfg\"\n{\n\t\"setting.defaultres\"\t\t\"2560\"\n\t\"setting.mat_vsync\"\t\t\"0\"\n}";
        let e = text(video.as_bytes(), "cs2_video.txt");
        assert_eq!(e["setting.defaultres"], "2560");
    }

    #[test]
    fn json_is_flattened() {
        let e = text(br#"{"video":{"fov":90,"modes":["a","b"]},"vsync":true}"#, "settings.json");
        assert_eq!(e["video/fov"], "90");
        assert_eq!(e["video/modes[1]"], "b");
        assert_eq!(e["vsync"], "true");
    }

    #[test]
    fn xml_settings_get_meaningful_keys() {
        let xml = "<?xml version=\"1.0\"?>\n<Settings>\n  <Shadow-Quality>HIGH</Shadow-Quality>\n  <shadowQuality value=\"kSettingLevel_High\" />\n</Settings>\n";
        let e = text(xml.as_bytes(), "system.xml");
        assert_eq!(e["Shadow-Quality"], "HIGH");
        assert_eq!(e["<shadowQuality />"], "kSettingLevel_High");
    }

    #[test]
    fn utf16_and_binary_detection() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "[A]\r\nx=1\r\n".encode_utf16() {
            bytes.extend(u.to_le_bytes());
        }
        assert_eq!(text(&bytes, "a.ini")["[A] x"], "1");
        assert_eq!(parse(&[0, 159, 146, 150], "a.ini"), Content::Binary);
        assert_eq!(parse(b"a\0b", "a.cfg"), Content::Binary);
    }

    #[test]
    fn diff_reports_added_removed_modified() {
        let old = parse_ini("a=1\nb=2\nc=3\n");
        let new = parse_ini("a=1\nb=5\nd=4\n");
        let d = diff(&old, &new);
        let keys: Vec<_> = d.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec!["b", "c", "d"]);
        assert_eq!(d[0].old.as_deref(), Some("2"));
        assert_eq!(d[0].new.as_deref(), Some("5"));
        assert_eq!(d[1].new, None);
        assert_eq!(d[2].old, None);
        assert_eq!(union_len(&old, &new), 4);
    }
}
