//! Filename rules: sanitising, templating and duplicate detection.
//!
//! Everything here is pure except [`dedupe_path`], which only asks the
//! filesystem whether a name is taken. The sanitizer implements *Windows* rules
//! because that is the target platform: nine reserved characters, control
//! characters, trailing dots and spaces, and the reserved device names.

use std::path::PathBuf;

use crate::DownloadRequest;

/// Character budget for one rendered file name, before the extension. NTFS
/// allows 255 UTF-16 units; 120 leaves room for the dedup suffix and the
/// extension while keeping the whole path short enough for `MAX_PATH`.
const MAX_NAME_CHARS: usize = 120;

/// Characters Windows forbids inside a single path component.
const ILLEGAL: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Make a string safe as a single filename component.
///
/// Strips Windows-reserved characters and names, collapses whitespace and
/// truncates on a character boundary so multi-byte titles cannot be split.
#[must_use]
pub fn sanitize_component(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(1024));
    let mut space_pending = false;
    for ch in input.chars() {
        let mapped = if ILLEGAL.contains(&ch) || ch.is_control() {
            '_'
        } else if ch.is_whitespace() {
            ' '
        } else {
            ch
        };
        if mapped == ' ' {
            // Collapse runs and drop leading whitespace.
            if !out.is_empty() {
                space_pending = true;
            }
            continue;
        }
        if space_pending {
            out.push(' ');
            space_pending = false;
        }
        out.push(mapped);
    }

    // `take(max_chars)` stops at a char boundary by construction: it walks
    // chars, never bytes, so a multi-byte title can never be split.
    let mut out: String = out.chars().take(max_chars).collect();
    trim_tail(&mut out);
    if !out.is_empty() && is_reserved_name(&out) {
        out.insert(0, '_');
        if out.chars().count() > max_chars {
            out = out.chars().take(max_chars).collect();
            trim_tail(&mut out);
            if out.is_empty() {
                out.push('_');
            }
        }
    }
    out
}

/// Expand a filename template for one request.
///
/// Known tokens are replaced; an unknown `{token}` is left verbatim so the user
/// can see what they typed instead of silently getting an empty name.
#[must_use]
pub fn render_filename(template: &str, request: &DownloadRequest) -> String {
    let expanded = expand_tokens(template, request);
    let (stem, extension) = split_extension(&expanded);
    let budget = MAX_NAME_CHARS
        .saturating_sub(extension.chars().count())
        .max(1);
    let mut stem = sanitize_component(stem, budget);
    if stem.is_empty() {
        stem = sanitize_component(&request.illust_id, budget);
    }
    if stem.is_empty() {
        stem.push('_');
    }
    format!("{stem}{extension}")
}

/// Full file name for one request: the rendered template plus the extension.
pub(crate) fn download_file_name(template: &str, request: &DownloadRequest) -> String {
    let stem = render_filename(template, request);
    let extension = sanitize_component(request.extension.trim_start_matches('.'), 16);
    if extension.is_empty() {
        return stem;
    }
    let already = stem
        .to_ascii_lowercase()
        .ends_with(&format!(".{}", extension.to_ascii_lowercase()));
    if already {
        stem
    } else {
        format!("{stem}.{extension}")
    }
}

/// Append ` (1)`, ` (2)`, ... until the path is unused.
#[must_use]
pub fn dedupe_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .filter(|e| !e.is_empty());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    for n in 1..=9999u32 {
        let name = match &extension {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path
}

fn trim_tail(value: &mut String) {
    // Windows silently refuses to create (or silently trims) a name that ends in
    // a dot or a space, so both are stripped here instead.
    while value.ends_with(' ') || value.ends_with('.') {
        value.pop();
    }
}

fn is_reserved_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches([' ', '.']);
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = upper.as_bytes();
    bytes.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && bytes[3].is_ascii_digit()
        && bytes[3] != b'0'
}

fn expand_tokens(template: &str, request: &DownloadRequest) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        match tail.find('}') {
            Some(close) => {
                let key = &tail[1..close];
                match token_value(key, request) {
                    Some(value) => out.push_str(&value),
                    None => out.push_str(&tail[..=close]),
                }
                rest = &tail[close + 1..];
            }
            None => {
                out.push_str(tail);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn token_value(key: &str, request: &DownloadRequest) -> Option<String> {
    match key {
        "illust_id" => Some(request.illust_id.clone()),
        "title" => Some(request.title.clone()),
        "author" => Some(request.author.clone()),
        "page" => Some(padded_page(request)),
        "page_count" => Some(request.page_count.to_string()),
        _ => None,
    }
}

/// `page_index + 1` zero-padded to the width of `page_count`, so `3` of `12`
/// becomes `03` and the files sort in reading order.
fn padded_page(request: &DownloadRequest) -> String {
    let width = request.page_count.max(1).to_string().len();
    let page = u64::from(request.page_index).saturating_add(1);
    format!("{page:0width$}")
}

/// Split a trailing `.<alnum>` extension off, so sanitising and truncating the
/// stem never eats the extension.
fn split_extension(value: &str) -> (&str, &str) {
    if let Some(pos) = value.rfind('.') {
        let extension = &value[pos + 1..];
        let width = extension.chars().count();
        if pos > 0
            && (1..=8).contains(&width)
            && extension.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return (&value[..pos], &value[pos..]);
        }
    }
    (value, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(page_index: u32, page_count: u32) -> DownloadRequest {
        DownloadRequest {
            illust_id: "1234567".to_string(),
            title: "初音ミク".to_string(),
            author: "画师".to_string(),
            page_index,
            page_count,
            url: "https://i.pximg.net/img-original/x.jpg".to_string(),
            extension: "jpg".to_string(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pixiv-download-naming-{}-{tag}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn replaces_windows_illegal_characters() {
        let cleaned = sanitize_component(r#"a<b>c:d"e/f\g|h?i*j"#, 64);
        assert_eq!(cleaned, "a_b_c_d_e_f_g_h_i_j");
        assert!(!cleaned.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*']));
    }

    #[test]
    fn replaces_control_characters_and_collapses_whitespace() {
        assert_eq!(sanitize_component("a\u{0}b\nc\td", 64), "a_b_c_d");
        assert_eq!(sanitize_component("  foo    bar   ", 64), "foo bar");
        // Control characters become underscores, even whitespace-like ones;
        // the trailing space is then trimmed away.
        assert_eq!(sanitize_component("\t\n ", 64), "__");
        assert_eq!(sanitize_component("abc\ndef", 64), "abc_def");
    }

    #[test]
    fn strips_trailing_dots_and_spaces() {
        assert_eq!(sanitize_component("name. . . ", 64), "name");
        assert_eq!(sanitize_component("name...", 64), "name");
        assert_eq!(sanitize_component("name ", 64), "name");
        // Interior dots survive: they are legal.
        assert_eq!(sanitize_component("a.b.c", 64), "a.b.c");
    }

    #[test]
    fn prefixes_reserved_device_names() {
        assert_eq!(sanitize_component("CON", 64), "_CON");
        assert_eq!(sanitize_component("con.txt", 64), "_con.txt");
        assert_eq!(sanitize_component("Aux", 64), "_Aux");
        assert_eq!(sanitize_component("nul", 64), "_nul");
        assert_eq!(sanitize_component("COM1", 64), "_COM1");
        assert_eq!(sanitize_component("lpt9.tar", 64), "_lpt9.tar");
        // Not reserved: COM0/LPT0 and anything that merely starts with "CON".
        assert_eq!(sanitize_component("COM0", 64), "COM0");
        assert_eq!(sanitize_component("CONSOLE", 64), "CONSOLE");
        assert_eq!(sanitize_component("PRN9", 64), "PRN9");
    }

    #[test]
    fn truncates_a_long_japanese_title_on_a_char_boundary() {
        let title = "初音ミクの消失".repeat(40);
        assert!(title.chars().count() > 120);
        let cleaned = sanitize_component(&title, 120);
        assert_eq!(cleaned.chars().count(), 120);
        // Still valid UTF-8 (String guarantees it) and no replacement chars.
        assert!(std::str::from_utf8(cleaned.as_bytes()).is_ok());
        assert_eq!(cleaned.len() % 3, 0, "3-byte chars must not be split");
        assert!(cleaned.starts_with("初音ミク"));
    }

    #[test]
    fn truncation_never_leaves_a_trailing_dot_or_space() {
        let cleaned = sanitize_component("abcdefghij klm.", 11);
        assert_eq!(cleaned, "abcdefghij");
    }

    #[test]
    fn render_expands_every_token() {
        let req = request(2, 12);
        assert_eq!(
            render_filename("{illust_id}_p{page}_{title}", &req),
            "1234567_p03_初音ミク"
        );
        assert_eq!(render_filename("{author}", &req), "画师");
        assert_eq!(render_filename("{page_count}", &req), "12");
        assert_eq!(render_filename("plain", &req), "plain");
    }

    #[test]
    fn render_zero_pads_the_page_to_the_width_of_page_count() {
        assert_eq!(render_filename("{page}", &request(2, 12)), "03");
        assert_eq!(render_filename("{page}", &request(9, 12)), "10");
        assert_eq!(render_filename("{page}", &request(0, 9)), "1");
        assert_eq!(render_filename("{page}", &request(6, 100)), "007");
        // A single-digit page count needs no padding.
        assert_eq!(render_filename("{page}", &request(0, 1)), "1");
        // Degenerate count still produces something usable.
        assert_eq!(render_filename("{page}", &request(0, 0)), "1");
    }

    #[test]
    fn render_leaves_unknown_tokens_verbatim() {
        assert_eq!(
            render_filename("{illust_id}{nope}_{page}", &request(0, 1)),
            "1234567{nope}_1"
        );
        assert_eq!(render_filename("{unclosed", &request(0, 1)), "{unclosed");
    }

    #[test]
    fn render_keeps_the_extension_and_stays_within_budget() {
        let mut req = request(0, 1);
        req.title = "長".repeat(400);
        let name = render_filename("{title}.jpg", &req);
        assert!(name.ends_with(".jpg"), "{name}");
        assert!(
            name.chars().count() <= 120,
            "{} chars",
            name.chars().count()
        );

        let bare = render_filename("{title}", &req);
        assert!(bare.chars().count() <= 120);
        assert!(!bare.contains('.'));
    }

    #[test]
    fn render_never_returns_an_empty_name() {
        let mut req = request(0, 1);
        req.title = "///".to_string();
        // Illegal characters become underscores rather than vanishing.
        assert_eq!(render_filename("{title}", &req), "___");
        req.title = "...".to_string();
        // Nothing usable is left, so the illustration id keeps the name unique.
        assert_eq!(render_filename("{title}", &req), "1234567");
        assert_eq!(render_filename("....", &req), "1234567");
    }

    #[test]
    fn file_name_appends_the_extension_once() {
        let req = request(2, 12);
        assert_eq!(
            download_file_name("{illust_id}_p{page}", &req),
            "1234567_p03.jpg"
        );
        assert_eq!(download_file_name("{illust_id}.jpg", &req), "1234567.jpg");
    }

    #[test]
    fn dedupe_path_appends_increasing_suffixes() {
        let dir = temp_dir("dedupe");
        let first = dir.join("123_p1.jpg");
        assert_eq!(
            dedupe_path(first.clone()),
            first,
            "a free path is returned as is"
        );

        std::fs::write(&first, b"x").expect("write");
        let second = dedupe_path(first.clone());
        assert_eq!(second, dir.join("123_p1 (1).jpg"));

        std::fs::write(&second, b"x").expect("write");
        assert_eq!(dedupe_path(first), dir.join("123_p1 (2).jpg"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedupe_path_handles_extensionless_names() {
        let dir = temp_dir("dedupe-noext");
        let first = dir.join("ugoira");
        std::fs::write(&first, b"x").expect("write");
        assert_eq!(dedupe_path(first), dir.join("ugoira (1)"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
