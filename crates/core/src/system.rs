//! Windows integration: autostart entry and the rights PresentMon needs.

use crate::error::{Error, Result};

/// Per-user autostart key. Writing here needs no administrator rights.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_VALUE: &str = "Blackbox";
/// Argument the app receives when started by the autostart entry.
pub const HIDDEN_ARG: &str = "--hidden";

/// Well-known SID of the built-in group "Performance Log Users". Its members
/// may start ETW sessions, which is what PresentMon needs.
pub const PERF_LOG_USERS_SID: &str = "S-1-5-32-559";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRights {
    /// Running elevated (as administrator).
    Elevated,
    /// Member of "Performance Log Users": capture works without elevation.
    PerformanceLogUsers,
    /// Frame capture will fail.
    None,
}

/// Evaluate the output of `whoami /groups /fo csv /nh`. Group names are
/// localized, SIDs are not.
pub fn parse_whoami_groups(output: &str) -> CaptureRights {
    // High (12288) or System (16384) mandatory level = elevated token.
    if output.contains("S-1-16-12288") || output.contains("S-1-16-16384") {
        CaptureRights::Elevated
    } else if output.contains(PERF_LOG_USERS_SID) {
        CaptureRights::PerformanceLogUsers
    } else {
        CaptureRights::None
    }
}

fn hidden_command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Rights of the current process for frame capture.
pub fn capture_rights() -> CaptureRights {
    match hidden_command("whoami").args(["/groups", "/fo", "csv", "/nh"]).output() {
        Ok(out) => parse_whoami_groups(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            tracing::warn!(error = %e, "whoami failed");
            CaptureRights::None
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = match chunk.len() {
            3 => (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]),
            2 => (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8),
            _ => u32::from(chunk[0]) << 16,
        };
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// PowerShell script that adds `user` to "Performance Log Users",
/// encoded for `-EncodedCommand` (base64 of UTF-16LE).
pub fn perf_log_users_script(user: &str) -> String {
    let script = format!(
        "Add-LocalGroupMember -SID '{PERF_LOG_USERS_SID}' -Member '{}' -ErrorAction SilentlyContinue",
        user.replace('\'', "''")
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&utf16)
}

/// Ask Windows (UAC prompt) to add the current user to "Performance Log
/// Users". Takes effect after the next sign-in.
pub fn request_capture_rights() -> Result<()> {
    let who = hidden_command("whoami").output().map_err(|e| Error::Other(format!("whoami: {e}")))?;
    let user = String::from_utf8_lossy(&who.stdout).trim().to_string();
    if user.is_empty() {
        return Err(Error::Other(crate::i18n::t("Benutzername konnte nicht ermittelt werden", "Could not determine the user name").into()));
    }
    let encoded = perf_log_users_script(&user);
    // The encoded script contains only base64 characters, so it is safe
    // inside the single-quoted argument list.
    let status = hidden_command("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '-NoProfile','-EncodedCommand','{encoded}'"
            ),
        ])
        .status()
        .map_err(|e| Error::Other(crate::tr!("PowerShell konnte nicht gestartet werden: {e}", "PowerShell could not be started: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Other(crate::i18n::t("Die Windows-Bestätigung wurde abgebrochen.", "The Windows confirmation was cancelled.").into()))
    }
}

/// Command line stored in the autostart entry.
pub fn autostart_command(exe: &std::path::Path) -> String {
    format!("\"{}\" {HIDDEN_ARG}", exe.display())
}

#[cfg(windows)]
pub fn read_run_entry(key_path: &str, name: &str) -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(key_path, KEY_READ)
        .ok()?
        .get_value::<String, _>(name)
        .ok()
}

/// Create (`Some`) or remove (`None`) an autostart entry.
#[cfg(windows)]
pub fn write_run_entry(key_path: &str, name: &str, command: Option<&str>) -> Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    let (key, _) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(key_path)?;
    match command {
        Some(cmd) => key.set_value(name, &cmd)?,
        None => match key.delete_value(name) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        },
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn read_run_entry(_key_path: &str, _name: &str) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn write_run_entry(_key_path: &str, _name: &str, _command: Option<&str>) -> Result<()> {
    Err(Error::Other(crate::i18n::t("Autostart wird nur unter Windows unterstützt", "Autostart is only supported on Windows").into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whoami_groups_are_evaluated_by_sid() {
        let elevated = r#""VORDEFINIERT\Administratoren","Alias","S-1-5-32-544","Verbindliche Gruppe"
"Verbindliche Beschriftung\Hohe Verbindlichkeitsstufe","Bezeichnung","S-1-16-12288","""#;
        assert_eq!(parse_whoami_groups(elevated), CaptureRights::Elevated);
        let perf = r#""VORDEFINIERT\Leistungsprotokollbenutzer","Alias","S-1-5-32-559","Verbindliche Gruppe"
"Mandatory Label\Medium Mandatory Level","Label","S-1-16-8192","""#;
        assert_eq!(parse_whoami_groups(perf), CaptureRights::PerformanceLogUsers);
        assert_eq!(parse_whoami_groups(r#""Jeder","Bekannte Gruppe","S-1-1-0","""#), CaptureRights::None);
    }

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn script_escapes_quotes_in_user_names() {
        let decoded_utf16: Vec<u16> = {
            let s = perf_log_users_script("PC\\O'Brien");
            // decode base64 back for the check
            let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut bits = 0u32;
            let mut n = 0;
            let mut bytes = Vec::new();
            for c in s.bytes().filter(|c| *c != b'=') {
                bits = (bits << 6) | table.iter().position(|t| *t == c).unwrap() as u32;
                n += 6;
                if n >= 8 {
                    n -= 8;
                    bytes.push((bits >> n) as u8);
                }
            }
            bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
        };
        let script = String::from_utf16(&decoded_utf16).unwrap();
        assert!(script.contains("-Member 'PC\\O''Brien'"));
        assert!(script.contains(PERF_LOG_USERS_SID));
    }

    #[test]
    fn autostart_command_quotes_path() {
        let cmd = autostart_command(std::path::Path::new(r"C:\Program Files\Blackbox\blackbox-app.exe"));
        assert_eq!(cmd, r#""C:\Program Files\Blackbox\blackbox-app.exe" --hidden"#);
    }

    #[cfg(windows)]
    #[test]
    fn run_entry_roundtrip_in_test_key() {
        let key = r"Software\BlackboxTest\Run";
        write_run_entry(key, "T", Some("x.exe --hidden")).unwrap();
        assert_eq!(read_run_entry(key, "T").as_deref(), Some("x.exe --hidden"));
        write_run_entry(key, "T", None).unwrap();
        write_run_entry(key, "T", None).unwrap(); // removing twice is fine
        assert_eq!(read_run_entry(key, "T"), None);
        let _ = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).delete_subkey_all(r"Software\BlackboxTest");
    }
}
