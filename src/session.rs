//! Session persistence.
//!
//! The Pixiv session is a bearer credential: anyone holding the `PHPSESSID`
//! value can act as the logged-in user. It therefore never goes into the SQLite
//! database, never into a log line, and never into the repository.
//!
//! On Windows the payload is sealed with DPAPI (`CryptProtectData`,
//! user-scoped), so a copy of the file is useless on another machine or another
//! account. Elsewhere the file is written with `0600` and a warning, because
//! there is no equivalent user-scoped keystore without pulling in another
//! dependency. The on-disk form is self-describing: a magic header says whether
//! the body is sealed.

use std::path::{Path, PathBuf};

use pixiv_api::Session;
use pixiv_types::Id;

/// Magic prefix for a DPAPI-sealed session file.
const MAGIC_SEALED: &[u8; 4] = b"PXD1";
/// Magic prefix for a plaintext session file (non-Windows fallback).
const MAGIC_PLAIN: &[u8; 4] = b"PXD0";

fn path_in(dir: &Path) -> PathBuf {
    dir.join("session.bin")
}

/// Serialise to the plaintext key=value form. No trailing newline dependency:
/// parsing tolerates a missing one.
pub fn serialise(session: &Session) -> String {
    let mut out = String::new();
    if let Some(cookie) = session.phpsessid.as_deref() {
        out.push_str("phpsessid=");
        out.push_str(cookie.trim());
        out.push('\n');
    }
    if let Some(user) = session.user_id {
        out.push_str(&format!("user_id={}\n", user.get()));
    }
    if let Some(name) = session.user_name.as_deref() {
        out.push_str("user_name=");
        out.push_str(name.trim());
        out.push('\n');
    }
    out
}

/// Parse the key=value form. Unknown keys are ignored, malformed values are
/// dropped rather than propagated: a corrupt line must not brick sign-in.
pub fn parse(raw: &str) -> Session {
    let mut session = Session::default();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "phpsessid" => session.phpsessid = Some(value.to_string()),
            "user_id" => session.user_id = value.parse::<u64>().ok().map(Id),
            "user_name" => session.user_name = Some(value.to_string()),
            _ => {}
        }
    }
    session
}

/// Load the stored session, or an empty one when there is nothing usable.
pub fn load(dir: &Path) -> Session {
    let Ok(blob) = std::fs::read(path_in(dir)) else {
        return Session::default();
    };
    if blob.len() < 4 {
        return Session::default();
    }
    let (magic, body) = blob.split_at(4);
    let plain = if magic == MAGIC_SEALED {
        match crypto::unprotect(body) {
            Some(bytes) => bytes,
            None => return Session::default(),
        }
    } else {
        body.to_vec()
    };
    parse(&String::from_utf8_lossy(&plain))
}

/// Persist the session. Returns an error only when the file could not be
/// written at all; sealing failure falls back to the plaintext form so the user
/// is never silently logged out on the next start.
pub fn save(dir: &Path, session: &Session) -> std::io::Result<()> {
    use std::io::Write;

    let plain = serialise(session);
    let mut blob = Vec::with_capacity(plain.len() + 4);
    match crypto::protect(plain.as_bytes()) {
        Some(sealed) => {
            blob.extend_from_slice(MAGIC_SEALED);
            blob.extend_from_slice(&sealed);
        }
        None => {
            blob.extend_from_slice(MAGIC_PLAIN);
            blob.extend_from_slice(plain.as_bytes());
        }
    }

    let path = path_in(dir);
    let mut file = std::fs::File::create(&path)?;
    file.write_all(&blob)?;
    file.sync_all()?;
    restrict(&path);
    Ok(())
}

/// Delete the stored session. Missing file is success.
pub fn clear(dir: &Path) {
    let _ = std::fs::remove_file(path_in(dir));
    // Remove the pre-DPAPI file name too, so an upgrade cannot leave a
    // plaintext cookie behind.
    let _ = std::fs::remove_file(dir.join("session"));
}

fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(target_os = "windows")]
mod crypto {
    //! Minimal DPAPI binding. Hand-rolled rather than pulling in `windows-sys`
    //! for two functions; the struct layout and calling convention are fixed by
    //! the Win32 ABI.

    use std::ffi::c_void;

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            data_descr: *const u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;

        fn CryptUnprotectData(
            data_in: *const DataBlob,
            data_descr: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }

    /// `CRYPTPROTECT_UI_FORBIDDEN`: never show a prompt from a background call.
    const UI_FORBIDDEN: u32 = 0x1;

    fn take(out: DataBlob) -> Vec<u8> {
        if out.pb_data.is_null() || out.cb_data == 0 {
            return Vec::new();
        }
        // SAFETY: DPAPI returned a buffer of exactly cb_data bytes allocated
        // with LocalAlloc, which we copy out and then release with LocalFree.
        let bytes = unsafe { std::slice::from_raw_parts(out.pb_data, out.cb_data as usize) };
        let owned = bytes.to_vec();
        unsafe {
            LocalFree(out.pb_data as *mut c_void);
        }
        owned
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let input = DataBlob {
            cb_data: data.len() as u32,
            pb_data: data.as_ptr() as *mut u8,
        };
        let mut out = DataBlob {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        // SAFETY: both blobs describe live buffers for the duration of the call.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return None;
        }
        Some(take(out))
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let input = DataBlob {
            cb_data: data.len() as u32,
            pb_data: data.as_ptr() as *mut u8,
        };
        let mut out = DataBlob {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        // SAFETY: as above; the description out-pointer is not requested.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return None;
        }
        Some(take(out))
    }
}

#[cfg(not(target_os = "windows"))]
mod crypto {
    //! No user-scoped keystore without another dependency; the caller writes the
    //! plaintext form with `0600` permissions instead.
    pub fn protect(_data: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn unprotect(_data: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            phpsessid: Some("12345_abcdef".to_string()),
            user_id: Some(Id(700_000_001)),
            user_name: Some("tester".to_string()),
        }
    }

    #[test]
    fn plaintext_round_trip() {
        let parsed = parse(&serialise(&session()));
        assert_eq!(parsed.phpsessid.as_deref(), Some("12345_abcdef"));
        assert_eq!(parsed.user_id.map(|id| id.get()), Some(700_000_001));
        assert_eq!(parsed.user_name.as_deref(), Some("tester"));
    }

    #[test]
    fn malformed_lines_are_dropped_not_propagated() {
        let parsed = parse("phpsessid=\nuser_id=not-a-number\nuser_name=\n=broken\nnoequals");
        assert_eq!(parsed.phpsessid, None);
        assert_eq!(parsed.user_id, None);
        assert_eq!(parsed.user_name, None);
    }

    #[test]
    fn whitespace_is_trimmed() {
        let parsed = parse("phpsessid=  abc  \nuser_name=  someone \n");
        assert_eq!(parsed.phpsessid.as_deref(), Some("abc"));
        assert_eq!(parsed.user_name.as_deref(), Some("someone"));
    }

    #[test]
    fn save_then_load_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("pxd-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &session()).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.phpsessid.as_deref(), Some("12345_abcdef"));
        assert_eq!(loaded.user_name.as_deref(), Some("tester"));
        assert!(loaded.is_authenticated());
        clear(&dir);
        assert_eq!(load(&dir).phpsessid, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_does_not_panic() {
        let dir = std::env::temp_dir().join(format!("pxd-session-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path_in(&dir), b"PX").unwrap();
        assert_eq!(load(&dir).phpsessid, None);
        std::fs::write(path_in(&dir), b"PXD1\x01\x02\x03\x04").unwrap();
        assert_eq!(load(&dir).phpsessid, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dpapi_seals_and_opens_the_payload() {
        let plain = b"phpsessid=secret-value";
        let sealed = crypto::protect(plain).expect("DPAPI available");
        assert_ne!(sealed, plain);
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(crypto::unprotect(&sealed).as_deref(), Some(&plain[..]));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_stored_session_is_not_readable_plaintext_on_disk() {
        let dir = std::env::temp_dir().join(format!("pxd-session-sealed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &session()).unwrap();
        let raw = std::fs::read(path_in(&dir)).unwrap();
        assert!(raw.starts_with(MAGIC_SEALED));
        assert!(
            !raw.windows(11).any(|w| w == b"12345_abcde"),
            "cookie must not appear in the file"
        );
        assert_eq!(load(&dir).phpsessid.as_deref(), Some("12345_abcdef"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
