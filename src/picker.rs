//! Native folder picker.
//!
//! The previous build answered "更改保存目录" by opening the *current* directory
//! in Explorer, which is not a picker: the user could not change anything. This
//! module shells out to the real Win32 shell dialog (`SHBrowseForFolderW` with
//! `BIF_NEWDIALOGSTYLE`, the resizable one), so the interaction is the same one
//! Explorer and the Windows 11 settings app give you.
//!
//! Must be called on the UI thread: the dialog is modal to the calling thread.

use std::path::PathBuf;

/// Show the folder chooser. Returns `None` when the user cancels or when the
/// platform has no such dialog (the caller keeps the current directory then).
#[cfg(windows)]
pub fn pick_folder(title: &str) -> Option<PathBuf> {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    struct BrowseInfoW {
        hwnd_owner: *mut c_void,
        pidl_root: *mut c_void,
        psz_display_name: *mut u16,
        lpsz_title: *const u16,
        ul_flags: u32,
        lpfn: *const c_void,
        l_param: isize,
        i_image: i32,
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHBrowseForFolderW(info: *const BrowseInfoW) -> *mut c_void;
        fn SHGetPathFromIDListW(pidl: *const c_void, path: *mut u16) -> i32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(mem: *mut c_void);
    }

    // Only existing filesystem directories, with an editable path box and the
    // resizable dialog frame.
    const BIF_RETURNONLYFSDIRS: u32 = 0x0000_0001;
    const BIF_EDITBOX: u32 = 0x0000_0010;
    const BIF_NEWDIALOGSTYLE: u32 = 0x0000_0040;
    /// `SHGetPathFromIDListW` writes at most MAX_PATH units.
    const MAX_PATH: usize = 260;

    let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let mut display = vec![0u16; MAX_PATH];
    let mut path = vec![0u16; MAX_PATH];

    let info = BrowseInfoW {
        hwnd_owner: std::ptr::null_mut(),
        pidl_root: std::ptr::null_mut(),
        psz_display_name: display.as_mut_ptr(),
        lpsz_title: title_wide.as_ptr(),
        ul_flags: BIF_RETURNONLYFSDIRS | BIF_EDITBOX | BIF_NEWDIALOGSTYLE,
        lpfn: std::ptr::null(),
        l_param: 0,
        i_image: 0,
    };

    // SAFETY: `info` outlives the call and its pointers reference live buffers.
    let pidl = unsafe { SHBrowseForFolderW(&info) };
    if pidl.is_null() {
        return None;
    }
    // SAFETY: `pidl` is a valid item id list owned by us until CoTaskMemFree.
    let ok = unsafe { SHGetPathFromIDListW(pidl, path.as_mut_ptr()) } != 0;
    unsafe { CoTaskMemFree(pidl) };
    if !ok {
        return None;
    }

    let len = path.iter().position(|unit| *unit == 0).unwrap_or(0);
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&path[..len])))
}

#[cfg(not(windows))]
pub fn pick_folder(_title: &str) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_symbol_resolves_on_this_platform() {
        // Compile/link check only: never opens a dialog in a test run.
        let f: fn(&str) -> Option<std::path::PathBuf> = super::pick_folder;
        let _ = f;
    }
}
