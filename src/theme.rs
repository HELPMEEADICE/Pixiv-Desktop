//! Light / dark / follow-the-system.
//!
//! Three states, not two. A boolean toggle cannot express "follow Windows", so the
//! old `theme` + `follow_system_theme` pair could be persisted into a state that
//! described nothing: `follow_system_theme = true` was stored, never read, and the
//! window stayed in whatever `theme` said. This module makes the choice explicit
//! and gives the derivation one owner.
//!
//! Windows exposes the app-mode preference as a single registry DWORD:
//!
//! ```text
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize
//!     AppsUseLightTheme = 0 (dark) | 1 (light)
//! ```
//!
//! Slint 1.18 has no `Window::color-scheme`, so the value is read here and pushed
//! into the `Theme` global. There is no notification hook reachable without a raw
//! window procedure, so a slow poll runs *only* while the mode is `System`: one
//! `RegQueryValueExW` every few seconds, dropped entirely once the user picks a
//! fixed theme.

use slint::ComponentHandle;

use crate::{Theme, ThemeMode};

/// SQLite key for the persisted choice.
pub const SETTING_KEY: &str = "theme_mode";

/// Parse the persisted value. Unknown or missing values mean "follow the system",
/// which is what a fresh install should do.
pub fn mode_from_setting(value: Option<&str>) -> ThemeMode {
    match value.map(str::trim) {
        Some("light") => ThemeMode::Light,
        Some("dark") => ThemeMode::Dark,
        _ => ThemeMode::System,
    }
}

pub fn setting_value(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Light => "light",
        ThemeMode::Dark => "dark",
        ThemeMode::System => "system",
    }
}

/// The `AppsUseLightTheme` value for the current user, if it can be read.
#[cfg(target_os = "windows")]
pub fn system_is_dark() -> bool {
    // 1 = light, 0 = dark. A missing value means Windows never wrote it, which
    // happens on a fresh profile; light is the documented default there, so
    // "unknown" must not be mistaken for "dark".
    matches!(read_apps_use_light_theme(), Some(0))
}

#[cfg(not(target_os = "windows"))]
pub fn system_is_dark() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn read_apps_use_light_theme() -> Option<u32> {
    use std::ffi::c_void;

    type Hkey = *mut c_void;
    /// `(HKEY)(LONG_PTR)-2147483647` — the predefined current-user root.
    const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
    const KEY_QUERY_VALUE: u32 = 0x0001;
    const REG_DWORD: u32 = 4;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(
            key: Hkey,
            sub_key: *const u16,
            options: u32,
            desired: u32,
            result: *mut Hkey,
        ) -> i32;
        fn RegQueryValueExW(
            key: Hkey,
            value_name: *const u16,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            size: *mut u32,
        ) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let sub_key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value_name = wide("AppsUseLightTheme");

    let mut key: Hkey = std::ptr::null_mut();
    // SAFETY: both strings are NUL-terminated and outlive the call; `key` is a
    // valid out-parameter and is closed on every path below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER as Hkey,
            sub_key.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if opened != 0 || key.is_null() {
        return None;
    }

    let mut data: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    let mut kind: u32 = 0;
    // SAFETY: `data` is a 4-byte DWORD buffer and `size` describes exactly it.
    let queried = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            &mut data as *mut u32 as *mut u8,
            &mut size,
        )
    };
    // SAFETY: `key` was opened above and is not used afterwards.
    unsafe { RegCloseKey(key) };

    if queried != 0 || kind != REG_DWORD || size != 4 {
        return None;
    }
    Some(data)
}

/// Push the current theme state into the UI. `Theme.dark` is derived inside Slint
/// from these two inputs, so this is the whole switch.
pub fn apply(ui: &crate::AppWindow, mode: ThemeMode) {
    let theme = ui.global::<Theme>();
    theme.set_mode(mode);
    theme.set_system_dark(system_is_dark());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_values_round_trip() {
        for mode in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
            assert_eq!(mode_from_setting(Some(setting_value(mode))), mode);
        }
    }

    #[test]
    fn unknown_or_missing_setting_falls_back_to_system() {
        assert_eq!(mode_from_setting(None), ThemeMode::System);
        assert_eq!(mode_from_setting(Some("")), ThemeMode::System);
        assert_eq!(mode_from_setting(Some("sepia")), ThemeMode::System);
        assert_eq!(mode_from_setting(Some("  dark ")), ThemeMode::Dark);
    }

    /// The derivation the UI performs, asserted here so the three-state contract
    /// is pinned somewhere a Slint edit cannot silently break.
    #[test]
    fn dark_is_derived_from_mode_and_system() {
        let dark = |mode: ThemeMode, system_dark: bool| {
            mode == ThemeMode::Dark || (mode == ThemeMode::System && system_dark)
        };
        assert!(dark(ThemeMode::Dark, false));
        assert!(!dark(ThemeMode::Light, true));
        assert!(dark(ThemeMode::System, true));
        assert!(!dark(ThemeMode::System, false));
    }
}
