//! Design-rule tests over the Slint source tree.
//!
//! These guard invariants that a compiler cannot check and that have already been
//! broken once each. They read the `.slint` files as text, so they cost nothing at
//! runtime and run in `cargo test` like any other unit test.
//!
//! They are deliberately about *rules*, not about exact numbers: a page may change
//! a padding without failing here, but it may not reach past the shared components
//! to place a glyph itself, and it may not name an icon that does not exist.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn ui_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")
    }

    /// Every `.slint` file under `ui/`, except the generated icon file.
    fn page_files() -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect(&ui_dir(), &mut files);
        files
            .into_iter()
            .filter(|path| path.file_name().and_then(|n| n.to_str()) != Some("icons.slint"))
            .collect()
    }

    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("slint") {
                out.push(path);
            }
        }
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Rule: a bare `FluentGlyph` may exist only inside the two glyph containers.
    ///
    /// A `Path` places itself at (0,0) regardless of the size it is given, and a
    /// fixed-height child of a `HorizontalLayout` is aligned to the start of the
    /// cross axis rather than centred. Both mistakes are invisible in review and
    /// both shipped here: every navigation glyph sat ~10 logical px above its
    /// label, and every `IconButton` glyph sat in the button's top-left corner.
    /// `IconPresenter` and `IconSlot` are therefore the only supported way to place
    /// one; using the raw element anywhere else is a regression by construction.
    #[test]
    fn raw_glyphs_exist_only_inside_the_glyph_containers() {
        let mut offenders = Vec::new();
        for path in page_files() {
            let source = read(&path);
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("").trim();
                if !code.contains("FluentGlyph {") {
                    continue;
                }
                let owner = enclosing_component(&source, index);
                if owner != "IconPresenter" && owner != "IconSlot" {
                    offenders.push(format!("{name}:{}: inside {owner}", index + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "place glyphs with IconSlot / IconPresenter, not the raw element:\n{}",
            offenders.join("\n")
        );
    }

    /// Name of the component a line belongs to: the nearest preceding
    /// `component X inherits ...` declaration in the same file.
    fn enclosing_component(source: &str, line: usize) -> String {
        let mut owner = String::from("<top level>");
        for (index, text) in source.lines().enumerate() {
            if index > line {
                break;
            }
            let code = text.split("//").next().unwrap_or("");
            if let Some(at) = code.find("component ") {
                let rest = &code[at + "component ".len()..];
                let ident: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !ident.is_empty() {
                    owner = ident;
                }
            }
        }
        owner
    }

    /// Rule: every icon name used anywhere in the UI exists in the generated set.
    ///
    /// An unknown name renders *nothing* — no error, no empty box, just a control
    /// whose icon silently disappeared. That is the worst possible failure mode for
    /// a typo, so it is checked here instead of being discovered on screen.
    #[test]
    fn every_referenced_icon_name_is_generated() {
        let icons = read(&ui_dir().join("icons.slint"));

        // `name == "home" ? IconPaths.home : ...` is how the generated dispatcher
        // enumerates valid names.
        let defined: BTreeSet<String> = icons
            .match_indices("? IconPaths.")
            .filter_map(|(index, _)| {
                let before = &icons[..index];
                let start = before.rfind('"')?;
                let head = &before[..start];
                let open = head.rfind('"')?;
                Some(before[open + 1..start].to_string())
            })
            .collect();
        assert!(
            defined.len() > 30,
            "icon table looks empty ({} entries); did icons.slint move?",
            defined.len()
        );

        let mut unknown = BTreeSet::new();
        for path in page_files() {
            let source = read(&path);
            // Only the value positions that take a glyph name, so prose in a
            // tooltip or a placeholder string cannot trip this.
            for key in ["icon", "name", "leading-icon", "glyph"] {
                for (index, _) in source.match_indices(key) {
                    let rest = &source[index + key.len()..];
                    let rest = rest.trim_start();
                    if !rest.starts_with(':') {
                        continue;
                    }
                    for quoted in quoted_strings(&rest[1..]) {
                        if quoted.is_empty()
                            || !quoted
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                        {
                            continue;
                        }
                        if !defined.contains(&quoted) {
                            unknown.insert(quoted);
                        }
                    }
                }
            }
        }
        assert!(
            unknown.is_empty(),
            "these icon names do not exist and would render as blank space: {unknown:?}"
        );
    }

    /// Rule: the theme is the only place that may name a raw colour.
    ///
    /// Pages are allowed a handful of deliberately fixed colours — the swatches
    /// drawn *on top of* artwork (rank badges, overlay scrims) must stay readable
    /// in both themes and therefore must not follow the theme. Everything else has
    /// to come from `Theme`, or dark mode quietly breaks one page at a time.
    #[test]
    fn pages_take_colours_from_the_theme() {
        // Each of these is an overlay drawn over artwork, where a themed colour
        // would be wrong: #ffffff on a dark scrim, #a6000000 for the scrim itself,
        // #d9ffffff for the floating bookmark pill, #2b2b2b for its glyph.
        const ARTWORK_OVERLAY: [&str; 4] = ["#ffffff", "#a6000000", "#d9ffffff", "#2b2b2b"];
        // Hover/press scrim drawn inside a cover.
        const COVER_SCRIM: [&str; 1] = ["#12000000"];

        let mut offenders = Vec::new();
        for path in page_files() {
            // `theme.slint` is where colours belong; every other file has to read
            // them from there.
            if path.file_name().and_then(|n| n.to_str()) == Some("theme.slint") {
                continue;
            }
            let source = read(&path);
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for (at, _) in code.match_indices('#') {
                    let rest = &code[at..];
                    let len = rest
                        .char_indices()
                        .take_while(|(_, c)| c.is_ascii_hexdigit() || *c == '#')
                        .count();
                    let literal = &rest[..len];
                    if literal.len() < 4 {
                        continue;
                    }
                    let allowed =
                        ARTWORK_OVERLAY.contains(&literal) || COVER_SCRIM.contains(&literal);
                    if !allowed {
                        offenders.push(format!("{name}:{}: {literal}", index + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "colours belong in theme.slint; only artwork overlays may be literal:\n{}",
            offenders.join("\n")
        );
    }

    /// Rule: text/background pairs that the layout actually puts together must
    /// clear WCAG AA contrast (4.5:1 for body text, 3:1 for large text and glyphs).
    ///
    /// The theme is the only place colours are defined, so it is also the only place
    /// contrast can be checked without rendering. This walks the real pairs the
    /// components use; a palette edit that makes a hint unreadable in dark mode fails
    /// here instead of shipping.
    #[test]
    fn theme_text_pairs_clear_wcag_aa() {
        let source = read(&ui_dir().join("theme.slint"));

        // `out property <color> name: dark ? #x : #y;` -> (dark, light) per name.
        let mut palette: std::collections::BTreeMap<String, (String, String)> =
            std::collections::BTreeMap::new();
        for line in source.lines() {
            let code = line.split("//").next().unwrap_or("");
            let Some(rest) = code.trim().strip_prefix("out property <color> ") else {
                continue;
            };
            let Some((name, expr)) = rest.split_once(':') else {
                continue;
            };
            let expr = expr.trim().trim_end_matches(';');
            match expr.strip_prefix("dark ?") {
                Some(branches) => {
                    let Some((dark, light)) = branches.split_once(':') else {
                        continue;
                    };
                    palette.insert(
                        name.trim().to_string(),
                        (
                            dark.trim().to_string(),
                            light.trim().trim_end_matches(';').trim().to_string(),
                        ),
                    );
                }
                // A colour that is the same in both themes (e.g. `on-accent`) is
                // still part of the palette and still has to clear contrast.
                None => {
                    palette.insert(
                        name.trim().to_string(),
                        (expr.to_string(), expr.to_string()),
                    );
                }
            }
        }
        assert!(
            palette.len() > 20,
            "theme palette looks empty ({} entries)",
            palette.len()
        );

        // (foreground, background, minimum ratio, description)
        // 4.5 = WCAG AA body text; 3.0 = large text and non-text UI.
        let pairs = [
            ("text", "canvas", 4.5, "body text on the page floor"),
            ("text", "layer", 4.5, "body text on a card"),
            ("text", "chrome", 4.5, "body text in the top bar / rail"),
            (
                "text-secondary",
                "canvas",
                4.5,
                "secondary text on the page floor",
            ),
            ("text-secondary", "layer", 4.5, "secondary text on a card"),
            (
                "text-tertiary",
                "canvas",
                4.5,
                "hint text on the page floor",
            ),
            ("text-tertiary", "layer", 4.5, "hint text on a card"),
            (
                "text-tertiary",
                "layer-alt",
                4.5,
                "hint text on an input / sunken row",
            ),
            // Filled controls: the *fill* token, not the decoration accent.
            ("on-accent", "accent-fill", 4.5, "label on a primary button"),
            // Text and glyphs on the accent wash (nav selection, chips, badges).
            (
                "accent-ink",
                "accent-subtle",
                4.5,
                "accent text on the selection wash",
            ),
            ("danger", "danger-subtle", 3.0, "error text in an InfoBar"),
            (
                "success",
                "success-subtle",
                3.0,
                "success text in an InfoBar",
            ),
            (
                "warning",
                "warning-subtle",
                3.0,
                "warning text in an InfoBar",
            ),
            ("tip-ink", "tip-surface", 4.5, "tooltip text"),
        ];

        // `accent` itself is deliberately absent: it is the exact Pixiv brand blue
        // (#0096fa), which measures 2.81:1 on the page floor - fine as a large
        // decorative fill, not fine as a glyph or text. `accent_is_decoration_only`
        // below enforces that no UI file reaches for it, so there is nothing to
        // assert here; `accent-fill` and `accent-ink` are what carry content.

        let mut failures = Vec::new();
        for theme_dark in [false, true] {
            for (fg, bg, minimum, what) in pairs {
                let (Some(fg), Some(bg)) = (palette.get(fg), palette.get(bg)) else {
                    failures.push(format!("missing palette entry for {what}"));
                    continue;
                };
                let fg_value = if theme_dark {
                    fg.0.as_str()
                } else {
                    fg.1.as_str()
                };
                let bg_value = if theme_dark {
                    bg.0.as_str()
                } else {
                    bg.1.as_str()
                };
                let (Some(fg_rgb), Some(bg_rgb)) = (parse_colour(fg_value), parse_colour(bg_value))
                else {
                    failures.push(format!(
                        "unparsable colour in {what}: {fg_value} / {bg_value}"
                    ));
                    continue;
                };
                let ratio = contrast_ratio(fg_rgb, bg_rgb);
                if ratio < minimum {
                    failures.push(format!(
                        "{what} ({fg_value} on {bg_value}) is {ratio:.2}:1, below {minimum}:1"
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "theme contrast failures:\n{}",
            failures.join("\n")
        );
    }

    /// Parse `#rgb`, `#rrggbb` or `#aarrggbb` Slint colour literals to (r, g, b).
    fn parse_colour(literal: &str) -> Option<(f64, f64, f64)> {
        let hex = literal.trim().strip_prefix('#')?;
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|v| v as u8))
            .collect::<Option<Vec<u8>>>()?;
        let (r, g, b) = match digits.len() {
            3 => (digits[0] * 17, digits[1] * 17, digits[2] * 17),
            6 | 8 => {
                // 8 digits carry a leading alpha, which contrast ignores.
                let base = if digits.len() == 8 { 2 } else { 0 };
                (
                    digits[base] * 16 + digits[base + 1],
                    digits[base + 2] * 16 + digits[base + 3],
                    digits[base + 4] * 16 + digits[base + 5],
                )
            }
            _ => return None,
        };
        Some((r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0))
    }

    /// WCAG 2.1 relative luminance and contrast ratio.
    fn contrast_ratio(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
        fn luminance((r, g, b): (f64, f64, f64)) -> f64 {
            fn channel(value: f64) -> f64 {
                if value <= 0.039_28 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        }
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Rule: `Theme.accent` is decoration only; content uses fill/ink variants.
    ///
    /// The Pixiv brand blue is #0096fa, which is 2.81:1 on the page floor and
    /// 3.11:1 on a card - below the 4.5:1 WCAG AA bar for text and below even the
    /// 3:1 bar for meaningful glyphs. Keeping the exact brand colour is a design
    /// requirement, so the fix is separation of roles: `accent` may only be a large
    /// decorative fill (the wordmark tile), `accent-fill` backgrounds anything with
    /// text or glyphs on it, and `accent-ink` is the legible variant for the accent
    /// wash. That is enforceable as absence: no UI file may name `Theme.accent`.
    #[test]
    fn accent_is_decoration_only() {
        let mut offenders = Vec::new();
        for path in page_files() {
            if path.file_name().and_then(|n| n.to_str()) == Some("theme.slint") {
                continue;
            }
            let source = read(&path);
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for (at, _) in code.match_indices("Theme.accent") {
                    let rest = &code[at + "Theme.accent".len()..];
                    let boundary = rest
                        .chars()
                        .next()
                        .map(|c| !c.is_ascii_lowercase() && c != '-')
                        .unwrap_or(true);
                    if boundary {
                        offenders.push(format!("{}:{}: {}", name, index + 1, code.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "use Theme.accent-fill for surfaces and Theme.accent-ink for text; Theme.accent is decoration only:\n{}",
            offenders.join("\n")
        );
    }

    /// Rule: a `key-pressed` comparison must use a Slint *character code*.
    ///
    /// Slint reports special keys as single character codes from `i-slint-common`'s
    /// key table - U+F702 for the left arrow, U+F70E for F11, U+001B for Escape -
    /// never as the English name. Comparing `e.text` against the word "LeftArrow"
    /// compiles, reads correctly and compares a one-character string against a
    /// thirteen-character one, so it is silently dead. That is exactly what had
    /// happened here: Alt+Left/Right and F11 were documented in the README and had
    /// never worked.
    ///
    /// A key literal is therefore only legal if it is a single character ("f",
    /// " ", "\n") or a backslash escape ("\u{001b}").
    #[test]
    fn key_handlers_compare_character_codes_not_key_names() {
        const MARKER: &str = "e.text == ";
        let mut offenders = Vec::new();
        for path in page_files() {
            let source = read(&path);
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                let Some(at) = code.find(MARKER) else {
                    continue;
                };
                let rest = code[at + MARKER.len()..].trim_start();
                let Some(rest) = rest.strip_prefix('"') else {
                    continue;
                };
                let Some(end) = rest.find('"') else {
                    continue;
                };
                let literal = &rest[..end];
                let is_single_char = literal.chars().count() == 1;
                let is_escape = literal.starts_with('\\');
                if !is_single_char && !is_escape {
                    offenders.push(format!("{name}:{}: {literal}", index + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "Slint reports special keys as character codes (U+F702 = left arrow); these comparisons can never match:\n{}",
            offenders.join("\n")
        );
    }
    /// Collect the double-quoted strings at the top level of an expression.
    ///
    /// Stops at the terminating `;` so a ternary's branches are all included but
    /// the next property is not.
    fn quoted_strings(expr: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = expr.char_indices();
        while let Some((_, c)) = chars.next() {
            match c {
                ';' => break,
                '"' => {
                    let mut value = String::new();
                    for (_, inner) in chars.by_ref() {
                        if inner == '"' {
                            break;
                        }
                        value.push(inner);
                    }
                    out.push(value);
                }
                _ => {}
            }
        }
        out
    }
}
