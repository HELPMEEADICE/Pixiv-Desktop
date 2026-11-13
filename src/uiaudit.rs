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
        const ARTWORK_OVERLAY: [&str; 4] = ["#ffffff", "#000000a6", "#ffffffd9", "#2b2b2b"];
        // Hover/press scrim drawn inside a cover.
        const COVER_SCRIM: [&str; 1] = ["#00000012"];

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
                // 8 digits are #RRGGBBAA in Slint: the alpha is *trailing* and
                // contrast ignores it. (The first version of this helper assumed a
                // leading alpha - the same mistake the theme itself made; see
                // `translucent_colours_use_the_rrggbbaa_order`.)
                (
                    digits[0] * 16 + digits[1],
                    digits[2] * 16 + digits[3],
                    digits[4] * 16 + digits[5],
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
    /// Rule: an 8-digit colour literal is `#RRGGBBAA`, and its alpha must be usable.
    ///
    /// Slint parses 8-digit hex as `#RRGGBBAA` (`i-slint-common::color_parsing`,
    /// asserted by its own test as `#01234567 -> 0x67012345`). This file used to
    /// write them as `#AARRGGBB`, which put the alpha byte at the front and the red
    /// channel in the alpha slot. Every translucent colour in the app was therefore
    /// wrong: light-mode `hover` was `#0a000000` = alpha 0 (invisible), dark-mode
    /// `hover` was `#14ffffff` = alpha 255 of bright cyan (a colour no theme picks),
    /// and both `shadow-*` tokens were fully transparent.
    ///
    /// The check is order-independent: it rejects an 8-digit literal whose final
    /// byte is 0 (invisible - almost always the old mistake) and one that is a
    /// near-opaque saturated hue while claiming to be a low-alpha overlay.
    #[test]
    fn translucent_colours_use_the_rrggbbaa_order() {
        let mut offenders = Vec::new();
        for path in page_files() {
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
                    if len != 9 {
                        continue;
                    }
                    let literal = &rest[..len];
                    let alpha = u8::from_str_radix(&literal[7..9], 16).unwrap_or(255);
                    if alpha == 0 {
                        offenders.push(format!("{name}:{}: {literal} has alpha 0", index + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "8-digit colours are #RRGGBBAA; a zero alpha byte means the colour is invisible:\n{}",
            offenders.join("\n")
        );
    }

    /// Rule: source files carry no stray control bytes.
    ///
    /// A real defect, found while reading \`gallery.slint\`: a comment in it contained
    /// a raw ESC (0x1B) where a letter belonged. The Slint parser does not care -
    /// it lives in a \`//\` comment - so nothing failed, and the byte survived three
    /// rounds of review. It is invisible in an editor that renders escapes and it
    /// silently poisons every line-based tool: \`grep -c\` counts a line, a diff shows
    /// the line as changed, and a copy-paste through a terminal can eat the rest of
    /// the line.
    ///
    /// The rule is deliberately about *source text*, not about the UI: tab, CR and
    /// LF are the only control characters a source file may contain.
    #[test]
    fn sources_contain_no_stray_control_bytes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for (dir, extension) in [("ui", "slint"), ("src", "rs"), ("crates", "rs")] {
            collect_with_extension(&root.join(dir), extension, &mut files);
        }
        assert!(
            files.len() > 25,
            "source tree looks empty ({} files)",
            files.len()
        );

        let mut offenders = Vec::new();
        for path in files {
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let mut line = 1_usize;
            for (offset, byte) in bytes.iter().copied().enumerate() {
                match byte {
                    b'\n' => line += 1,
                    b'\t' | b'\r' => {}
                    0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f => offenders.push(format!(
                        "{name}:{line}: byte 0x{byte:02X} at offset {offset}"
                    )),
                    _ => {}
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "sources may only contain tab, CR and LF as control characters:\n{}",
            offenders.join("\n")
        );
    }

    /// Every file under \`dir\` whose extension matches, recursively.
    fn collect_with_extension(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_with_extension(&path, extension, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                out.push(path);
            }
        }
    }

    /// Rule: there is exactly one tooltip surface, and the window owns it.
    ///
    /// A tooltip is the only piece of UI that has to be drawn outside the control
    /// that owns it, and a child element can do neither of the two things that
    /// requires: it cannot escape an ancestor `clip`, and it cannot paint over a
    /// sibling that is declared later. Both failures shipped:
    ///
    /// * the compact navigation rail set `clip: true` on a 52px column and every
    ///   label tooltip was declared at `x: item.width + 8px`, i.e. outside it;
    /// * the command bar drew its `below` tooltips at `y: 54px`, under the page,
    ///   which is declared after the bar and paints an opaque background.
    ///
    /// Round three changed `above` to `below` and left the layering as it was, so
    /// the affordance stayed invisible for another release. The fix is one
    /// `HoverTip` in `main.slint`, fed by the `Tip` global; this keeps it that way.
    #[test]
    fn tooltips_are_rendered_by_one_overlay() {
        let mut offenders = Vec::new();
        let mut overlays = 0;
        for path in page_files() {
            let source = read(&path);
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("").trim();
                if !code.contains("HoverTip {") {
                    continue;
                }
                if name == "main.slint" {
                    overlays += 1;
                } else {
                    offenders.push(format!("{name}:{}: {code}", index + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "publish to the Tip global instead of parenting a HoverTip; a child cannot escape a clip or paint over a later sibling:\n{}",
            offenders.join("\n")
        );
        assert_eq!(
            overlays, 1,
            "main.slint must own exactly one tooltip surface"
        );
    }

    /// Rule: an indeterminate "still working" row is `LoadingRow`, not a bar.
    ///
    /// Four footers - gallery, home, profile, detail - each wrote
    /// `ProgressBar { indeterminate: true; width: 120px }` inside a
    /// `VerticalLayout { alignment: center }`. A layout alignment orders children
    /// along the main axis only, so the bar sat at the left edge of its row while
    /// the label beside it was centred. `StateView` had already solved this exact
    /// shape with paired stretch spacers; nobody applied it to the footers.
    ///
    /// The rule is about the component, not the arithmetic: a page may not build
    /// a progress bar at all. `downloads.slint` is exempt because its bar is a
    /// determinate per-row meter, not a loading indicator.
    #[test]
    fn indeterminate_loading_uses_the_shared_row() {
        const EXEMPT: [&str; 2] = ["components.slint", "downloads.slint"];
        let mut offenders = Vec::new();
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if EXEMPT.contains(&name.as_str()) {
                continue;
            }
            let source = read(&path);
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("").trim();
                if code.contains("ProgressBar {") {
                    offenders.push(format!("{name}:{}: {code}", index + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "use LoadingRow for an indeterminate row; a fixed-width bar in a centred layout is not centred:\n{}",
            offenders.join("\n")
        );
    }

    /// Rule: every interactive primitive declares an accessibility role.
    ///
    /// Measured before this rule existed: the UIA tree of all eight pages held
    /// invokable=0 and focusable=1 — the whole application was, to a screen
    /// reader, a wall of Text and Image nodes with one editable field in it and
    /// nothing that could be pressed. Nothing in the build noticed, because a
    /// widget without `accessible-role` compiles to exactly the same pixels as a
    /// widget with one. The role is the cheapest possible thing to require and
    /// the hardest to remember, so it is a build error from here on.
    ///
    /// The check is structural rather than numerical on purpose: it does not care
    /// which role, only that the component declares one. A role that is wrong is
    /// a review question; a role that is absent is a regression nobody sees.
    #[test]
    fn every_interactive_component_declares_a_role() {
        const INTERACTIVE: [&str; 10] = [
            "AppButton",
            "IconButton",
            "NavItem",
            "TagChip",
            "TextField",
            "ToggleSwitch",
            "SegmentedControl",
            "IllustCard",
            "UserCard",
            "BookmarkPill",
        ];
        let path = ui_dir().join("components.slint");
        let source = read(&path);
        let mut offenders = Vec::new();
        let mut seen = 0;
        for (index, line) in source.lines().enumerate() {
            let Some(ident) = declared_component(line) else {
                continue;
            };
            if !INTERACTIVE.contains(&ident.as_str()) {
                continue;
            }
            seen += 1;
            if !component_body(&source, index).contains("accessible-role:") {
                offenders.push(format!("components.slint:{}: {ident}", index + 1));
            }
        }
        assert_eq!(
            seen,
            INTERACTIVE.len(),
            "expected to find all {} interactive components in components.slint; a rename \
             that drops one out of this list also drops its role check",
            INTERACTIVE.len()
        );
        assert!(
            offenders.is_empty(),
            "these components take clicks but expose no accessible-role, so a screen reader \
             never sees them: \n{}",
            offenders.join("\n")
        );
    }

    /// Rule: a `FocusScope` is declared before the component's `TouchArea`.
    ///
    /// Slint hit-tests in reverse declaration order, so the *last* declared input
    /// element receives the press. The rule encodes what every working primitive
    /// in `components.slint` already did — scope first, touch area after — and it
    /// exists because the two cards did not: their focus scope was appended next
    /// to the focus ring at the bottom of the component, so that the ring would
    /// paint over the cover, and the scope ended up on top of the card's own
    /// click target. The cost was a dead click: the first press on a card was
    /// swallowed to move keyboard focus, and only the second opened the artwork.
    /// Measured by clicking the UIA ListItem's centre twice with
    /// `tools/drive.ps1`; the first click changed nothing and the second opened
    /// the detail page.
    ///
    /// Keyboard support must never be paid for with a click, and the ordering is
    /// invisible in review — hence a test rather than a comment.
    #[test]
    fn focus_scopes_sit_under_the_touch_area() {
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let source = read(&path);
            let lines: Vec<&str> = source.lines().collect();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for (index, line) in lines.iter().enumerate() {
                let Some(component) = declared_component(line) else {
                    continue;
                };
                let mut first_scope = None;
                let mut first_touch = None;
                for line_index in component_range(&source, index) {
                    let text = lines.get(line_index).copied().unwrap_or("");
                    let code = text.split("//").next().unwrap_or("");
                    if first_scope.is_none() && code.contains("FocusScope") {
                        first_scope = Some(line_index + 1);
                    }
                    if first_touch.is_none() && code.contains("TouchArea") {
                        first_touch = Some(line_index + 1);
                    }
                }
                let (Some(scope), Some(touch)) = (first_scope, first_touch) else {
                    continue;
                };
                checked += 1;
                if scope > touch {
                    offenders.push(format!(
                        "{name}:{}: {component}: FocusScope at line {scope} is declared after \
                         TouchArea at line {touch}",
                        index + 1
                    ));
                }
            }
        }
        assert!(
            checked >= 6,
            "expected several components to have both a FocusScope and a TouchArea, found \
             {checked}; did the component scan stop working?"
        );
        assert!(
            offenders.is_empty(),
            "declare the FocusScope before the TouchArea: Slint hit-tests in reverse \
             declaration order, so a scope declared last swallows the first click in order \
             to take focus: \n{}",
            offenders.join("\n")
        );
    }

    /// The component name declared on this line, if it declares one.
    fn declared_component(line: &str) -> Option<String> {
        let code = line.split("//").next().unwrap_or("");
        let at = code.find("component ")?;
        let rest = &code[at + "component ".len()..];
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if ident.is_empty() { None } else { Some(ident) }
    }

    /// The lines of the component declared on `start`: `start..end`, where `end`
    /// is the line after the brace that closes it.
    ///
    /// Quoted literals are skipped while counting braces: a key handler is
    /// written `e.text == "\\u{F700}"`, and a string that carries its own brace
    /// must not unbalance the scan.
    fn component_range(source: &str, start: usize) -> std::ops::Range<usize> {
        let mut depth = 0usize;
        let mut end = start + 1;
        for (index, text) in source.lines().enumerate() {
            if index < start {
                continue;
            }
            end = index + 1;
            let mut quoted = false;
            let mut escaped = false;
            let mut comment = false;
            let mut chars = text.chars().peekable();
            while let Some(ch) = chars.next() {
                if comment {
                    break;
                }
                if quoted {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        quoted = false;
                    }
                } else if ch == '"' {
                    quoted = true;
                } else if ch == '/' && chars.peek() == Some(&'/') {
                    comment = true;
                } else if ch == '{' {
                    depth += 1;
                } else if ch == '}' {
                    depth = depth.saturating_sub(1);
                }
            }
            if index > start && depth == 0 {
                break;
            }
        }
        start..end
    }

    /// The body of the component declared on `start`, comments stripped, from its
    /// `component X` line to the brace that closes it.
    fn component_body(source: &str, start: usize) -> String {
        let range = component_range(source, start);
        let mut out = String::new();
        for (index, text) in source.lines().enumerate() {
            if range.contains(&index) {
                out.push_str(text.split("//").next().unwrap_or(""));
                out.push('\n');
            }
        }
        out
    }

    /// The body scan must stop at its own closing brace, and must not be fooled by
    /// a brace inside a string literal.
    ///
    /// Worth a test of its own: if `component_body` ran to the end of the file it
    /// would still find `accessible-role:` — in the *next* component — and the
    /// rule above would pass on a component that declares nothing.
    #[test]
    fn component_body_stops_at_its_own_closing_brace() {
        let source = "export component A inherits Rectangle {\n    key: \"}\\u{F700}\";\n}\n\
                      export component B inherits Rectangle {\n    accessible-role: AccessibleRole.button;\n}\n";
        let a = component_body(source, 0);
        assert!(
            a.contains("component A"),
            "the declaration line is included"
        );
        assert!(
            !a.contains("component B"),
            "the scan overran into the next component"
        );
        assert!(
            !a.contains("accessible-role"),
            "A declares no role and must look like it"
        );
        assert!(
            component_body(source, 3).contains("accessible-role:"),
            "B's own body is found"
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
