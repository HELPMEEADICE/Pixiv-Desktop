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
        let palette = theme_palette();

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
            // The cancelled-download badge paired text-tertiary with layer-sunken:
            // 4.32:1, under AA for an 11 px label, and the table above measured only
            // canvas, layer and layer-alt, so nothing failed. The row now carries
            // text-secondary (5.58:1); the pair below is what pins it.
            ("text-secondary", "layer-sunken", 4.5, "ink on a sunken row"),
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

        // Pairs whose colours are written as literals rather than palette names.
        // These are artwork overlays, which must keep fixed colours in both themes
        // (see `ARTWORK_OVERLAY` above), so their contrast cannot be derived from
        // `theme.slint` at all — which is how U2 shipped. That pair is this one,
        // and nothing measured it: the pill's hover surface used to be
        // `Theme.layer`, which *is* `#2b2b2b` in dark mode, drawn behind a
        // `#2b2b2b` glyph. 1.00:1, no rendering needed to know.
        let literal_pairs = [
            (
                "#2b2b2b",
                "#ffffffd9",
                3.0,
                "bookmark glyph on its pill surface",
            ),
            (
                "#0071c2",
                "#ffffffd9",
                4.5,
                "marked bookmark glyph on its pill surface",
            ),
        ];

        let mut failures = Vec::new();
        for theme_dark in [false, true] {
            for &(fg, bg, minimum, what) in pairs.iter().chain(literal_pairs.iter()) {
                let (Some(fg_value), Some(bg_value)) = (
                    resolve(fg, &palette, theme_dark),
                    resolve(bg, &palette, theme_dark),
                ) else {
                    failures.push(format!("missing palette entry for {what} ({fg} / {bg})"));
                    continue;
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

    /// The theme palette: `out property <color> name: dark ? #x : #y;` ->
    /// (dark, light) per name. A colour that is the same in both themes (e.g.
    /// `on-accent`) is still part of the palette and still has to clear contrast.
    fn theme_palette() -> std::collections::BTreeMap<String, (String, String)> {
        let source = read(&ui_dir().join("theme.slint"));

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
        palette
    }

    /// One side of a contrast pair resolved for one theme: a palette token's
    /// value, or a literal used exactly as written. Literals are how the artwork
    /// overlays are stated, and they are the only colours the theme cannot speak
    /// for.
    fn resolve<'a>(
        name: &'a str,
        palette: &'a std::collections::BTreeMap<String, (String, String)>,
        dark: bool,
    ) -> Option<&'a str> {
        if name.starts_with('#') {
            return Some(name);
        }
        let (dark_value, light_value) = palette.get(name)?;
        Some(if dark {
            dark_value.as_str()
        } else {
            light_value.as_str()
        })
    }

    /// Every colour an expression paints with: `#rrggbb[aa]` literals as written,
    /// and `Theme.<token>` references by their palette name.
    ///
    /// Stops at the terminating `;`, so the next property never leaks in.
    fn colours_in(expr: &str) -> Vec<String> {
        let expr = expr.split(';').next().unwrap_or(expr);
        let mut out = Vec::new();
        let mut rest = expr;
        while let Some(at) = rest.find('#') {
            let hex: String = rest[at..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '#')
                .collect();
            rest = &rest[at + hex.len()..];
            if hex.len() >= 4 {
                out.push(hex);
            }
        }
        let mut rest = expr;
        while let Some(at) = rest.find("Theme.") {
            rest = &rest[at + "Theme.".len()..];
            let ident: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if ident.is_empty() {
                break;
            }
            rest = &rest[ident.len()..];
            out.push(ident);
        }
        out
    }

    /// Rule: every surface the bookmark pill paints clears the glyph it carries.
    ///
    /// U2, verbatim: dark-mode hover set the pill's surface to `Theme.layer` while
    /// the unmarked glyph is a fixed `#2b2b2b` — and `Theme.layer` *is* `#2b2b2b`
    /// in dark. Contrast 1.00:1, so the glyph vanished, in the one state a pointer
    /// produces and no capture outside a hover ever shows. Both colours are
    /// deliberately fixed artwork overlays, so the palette table alone cannot see
    /// the pair; this reads what the component paints instead. The rule is that
    /// *no* surface the pill paints may reach its own ink, in either theme.
    #[test]
    fn bookmark_pill_keeps_its_glyph_visible() {
        let palette = theme_palette();
        let source = read(&ui_dir().join("components.slint"));
        let Some(start) = source
            .lines()
            .position(|line| declared_component(line).as_deref() == Some("BookmarkPill"))
        else {
            panic!("components.slint declares no BookmarkPill; this rule has nothing to check");
        };
        let body = component_body(&source, start);

        // The ink that does not follow the theme: the fixed literal the `tint`
        // falls back to when unmarked. (`Theme.accent-fill` there is the *marked*
        // ink — a question about that token, not about the surface.)
        let mut ink: Vec<String> = Vec::new();
        // Every colour the pill paints its own surface with, hover included.
        let mut surfaces: Vec<String> = Vec::new();
        for line in body.lines() {
            let Some((name, expr)) = line.trim().split_once(':') else {
                continue;
            };
            let colours = colours_in(expr);
            match name.trim() {
                // Every colour the glyph can be painted with, themed or literal:
                // the marked ink used to be `Theme.accent-fill`, which is #4cb0f5 in
                // dark mode and 2.38:1 on the pill. Filtering to literals hid it.
                "tint" => ink.extend(colours),
                "background" => surfaces.extend(colours),
                _ => {}
            }
        }
        assert!(!ink.is_empty(), "the pill paints no fixed glyph colour");
        assert!(!surfaces.is_empty(), "the pill paints no surface");

        let mut failures = Vec::new();
        for theme_dark in [false, true] {
            let mode = if theme_dark { "dark" } else { "light" };
            for ink_name in &ink {
                let Some(ink_rgb) = resolve(ink_name, &palette, theme_dark).and_then(parse_colour)
                else {
                    failures.push(format!("unresolvable glyph colour {ink_name}"));
                    continue;
                };
                for surface_name in &surfaces {
                    let Some(surface_rgb) =
                        resolve(surface_name, &palette, theme_dark).and_then(parse_colour)
                    else {
                        failures.push(format!("unresolvable pill surface {surface_name}"));
                        continue;
                    };
                    let ratio = contrast_ratio(ink_rgb, surface_rgb);
                    if ratio < 3.0 {
                        failures.push(format!(
                            "{mode}: glyph {ink_name} on surface {surface_name} is {ratio:.2}:1"
                        ));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "a pill whose surface reaches its own glyph is an invisible control; signal hover              with the border, never with a surface that can equal the ink:
{}",
            failures.join("
")
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

    /// Rule: a component that draws text on `Theme.accent-fill` inks it with `on-accent`.
    ///
    /// The theme states the pair and its measurement in the doc comment of
    /// `accent-fill`: white on the dark value `#4cb0f5` is 2.38:1, `on-accent` on it
    /// is 8.01:1. The ranking badge painted the literal `#ffffff` on that fill for a
    /// whole round and this file could not see it: `#ffffff` is a legitimate
    /// artwork-overlay ink (the type badge on a cover), so it sits in the allowlist,
    /// and the pair was never in the WCAG table above. This rule asks the question
    /// the table cannot: if a component fills a surface with the accent, what colour
    /// does it put on top?
    ///
    /// Round ten took the last hex literal off that fill (round ten U9: the six
    /// artwork-overlay literals moved into the theme), which would have emptied this
    /// rule's sample and turned its own non-vacuity assertion red. The sample is
    /// therefore no longer *a literal ink on the accent fill* but *any ink on the
    /// accent fill* - the question that can still be answered wrongly once the ink
    /// is a token. The two conditions that kept the false positives out are the ones
    /// that matter and they are unchanged: a `background:` fill (not a border, not a
    /// selection bar) **and** text drawn through one of the app's text primitives.
    #[test]
    fn text_drawn_on_the_accent_fill_uses_the_ink_token() {
        let source = read(&ui_dir().join("components.slint"));
        let mut offenders = Vec::new();
        let mut checked = 0;
        for (index, line) in source.lines().enumerate() {
            let Some(component) = declared_component(line) else {
                continue;
            };
            let body = component_body(&source, index);
            // A *surface*, not any mention: `accent-fill` is also a border and a
            // focused-ring colour, and asking about every mention flagged five
            // components that ink nothing on it. The question is what sits *on* the
            // fill, and that is decided structurally: the text primitive has to be
            // inside the same `{ }` block as the `background:` line. Indentation
            // cannot answer it - Slint puts a component's properties and its children
            // at the same level, so the ranking badge's fill and its `rank-label` start
            // in the same column. Measured against this tree the answer is one
            // component, and not the three shapes that made earlier versions of this
            // rule wrong: TextField's 2 px focus rule, NavItem's 3 px selection bar and
            // its 8 px compact badge are all `background: Theme.accent-fill` inside a
            // block that draws no text.
            let lines: Vec<&str> = source.lines().collect();
            let block_of = |start: usize| -> (usize, usize) {
                // Walk outwards from the fill line to the `{` that opened its enclosing
                // block, then forwards to that block's matching `}`. Iterating the lines
                // rather than a range keeps clippy's `needless_range_loop` satisfied and
                // says what the loop is for.
                let mut depth = 0isize;
                let mut open = start;
                for (at, line) in lines.iter().enumerate().take(start + 1).rev() {
                    let code = line.split("//").next().unwrap_or("");
                    depth += code.matches('}').count() as isize;
                    depth -= code.matches('{').count() as isize;
                    if depth < 0 {
                        open = at;
                        break;
                    }
                }
                let mut depth = 0isize;
                let mut close = start;
                for (at, line) in lines.iter().enumerate().skip(open) {
                    let code = line.split("//").next().unwrap_or("");
                    depth += code.matches('{').count() as isize;
                    depth -= code.matches('}').count() as isize;
                    if depth <= 0 && at > open {
                        close = at;
                        break;
                    }
                }
                (open, close)
            };
            for line_index in component_range(&source, index) {
                let code = lines[line_index].split("//").next().unwrap_or("");
                if !(code.contains("background:") && code.contains("Theme.accent-fill")) {
                    continue;
                }
                let (open, close) = block_of(line_index);
                let draws_text = lines[open..=close].iter().any(|line| {
                    let code = line.split("//").next().unwrap_or("");
                    code.contains("Text {")
                        || code.contains("Label {")
                        || code.contains("BodyText {")
                });
                if !draws_text {
                    continue;
                }
                checked += 1;
                if !body.contains("Theme.on-accent") {
                    offenders.push(format!("components.slint:{}: {component}", index + 1));
                }
            }
        }
        assert!(
            checked > 0,
            "expected at least one component to fill a surface with the accent and draw text \
             on it; a rule that checks nothing is not a rule"
        );
        assert!(
            offenders.is_empty(),
            "these components draw text on the accent fill without on-accent ink: {:?}",
            offenders
        );
    }

    /// Rule: an interactive component is reachable from the keyboard.
    ///
    /// The rule above proves a click target is *named*; it says nothing about
    /// whether Tab can reach it, and two controls could not be reached at all.
    /// `ToggleSwitch` and `SegmentedControl` declared no `FocusScope`, and Slint
    /// focuses nothing else, so the demo-mode switch and the three-way theme
    /// selector were pointer-only in a shell whose other primitives walk cleanly.
    /// `TextField` is the one exception by construction: a `TextInput` is natively
    /// focusable, so its body contains `TextInput` and needs no scope.
    #[test]
    fn every_interactive_component_can_take_focus() {
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
        let source = read(&ui_dir().join("components.slint"));
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
            // `BookmarkPill` is the one interactive component with no scope, and
            // deliberately so: it is a per-card shortcut, and one Tab stop per card
            // would make a 40-item grid 80 stops before roving focus exists (round
            // eight phase 2 lists it as U11). The same action already has a
            // keyboard path — the detail page's InfoPanel bookmark button — so no
            // user is locked out of the capability. The exception goes when roving
            // focus lands, not before.
            if ident == "BookmarkPill" {
                continue;
            }
            let body = component_body(&source, index);
            if !body.contains("FocusScope") && !body.contains("TextInput") {
                offenders.push(format!("components.slint:{}: {ident}", index + 1));
            }
        }
        assert_eq!(
            seen,
            INTERACTIVE.len(),
            "keep this list in step with every_interactive_component_declares_a_role"
        );
        assert!(
            offenders.is_empty(),
            "these components cannot be reached from the keyboard: {:?}",
            offenders
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

    /// Rule: a declared motion token is referenced, and no `animate` invents its
    /// own duration.
    ///
    /// `theme.slint` has declared `motion-fast: 90ms` and `motion: 150ms` since
    /// round three, and for two rounds the whole UI contained exactly one
    /// `animate` (the toggle-switch thumb) while `Theme.motion` was read by
    /// nothing at all. A token table nothing reads is not a design language, it is
    /// a comment: the rail snapped, a page appeared between two frames, a cover
    /// popped into a card and the InfoBar blinked, and none of that showed in
    /// review because the durations the theme promised never appeared in a diff.
    ///
    /// Both halves are about *drift*, not about values. A `<duration>` token no
    /// other file reads is a rule that silently stopped applying; an `animate`
    /// that writes `duration: 120ms` bypasses the table and the 150 ms ceiling the
    /// style contract sets. The reference check is deliberately not a plain
    /// `contains`: `Theme.motion` is a *prefix* of `Theme.motion-fast`, so a text
    /// search would let a dead `motion` token pass on its sibling's strength,
    /// which is the exact shape of the mistake this rule exists to catch.
    #[test]
    fn every_declared_motion_token_is_referenced_and_no_animate_uses_a_literal_duration() {
        let theme = read(&ui_dir().join("theme.slint"));

        // (a) `out property <duration> motion-fast: 90ms;`
        let mut declared: Vec<(String, usize)> = Vec::new();
        for (index, line) in theme.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("").trim();
            let Some(rest) = code.strip_prefix("out property <duration> ") else {
                continue;
            };
            let Some(colon) = rest.find(':') else {
                continue;
            };
            let name = rest[..colon].trim();
            if !name.is_empty() {
                declared.push((name.to_string(), index + 1));
            }
        }
        assert!(
            declared.len() >= 2,
            "theme.slint should declare the motion tokens, found {}",
            declared.len()
        );

        // (b) each token is read from at least one file that is not the theme.
        let others: Vec<(String, String)> = page_files()
            .into_iter()
            .filter(|path| path.file_name().and_then(|n| n.to_str()) != Some("theme.slint"))
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                (name, read(&path))
            })
            .collect();
        let mut unused = Vec::new();
        for (token, line) in &declared {
            if !others.iter().any(|(_, source)| reads_token(source, token)) {
                unused.push(format!(
                    "theme.slint:{line}: Theme.{token} is declared but no other file reads it"
                ));
            }
        }
        assert!(
            unused.is_empty(),
            "a motion token nothing reads is a rule that stopped applying:\n{}",
            unused.join("\n")
        );

        // (c) every `duration:` inside an `animate` block names a token.
        let mut literals = Vec::new();
        for (name, source) in &others {
            for (line, value) in animate_durations(source) {
                if !value.starts_with("Theme.") {
                    literals.push(format!("{name}:{line}: duration: {value}"));
                }
            }
        }
        assert!(
            literals.is_empty(),
            "an animation takes its duration from Theme.motion-fast or Theme.motion:\n{}",
            literals.join("\n")
        );
    }

    /// True when `source` reads `Theme.<token>`, and not a token that merely
    /// starts with it: `Theme.motion` is a prefix of `Theme.motion-fast`.
    fn reads_token(source: &str, token: &str) -> bool {
        let needle = format!("Theme.{token}");
        source.lines().any(|line| {
            let code = line.split("//").next().unwrap_or("");
            code.match_indices(&needle).any(|(at, _)| {
                !matches!(
                    code[at + needle.len()..].chars().next(),
                    Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_'
                )
            })
        })
    }

    /// Every `duration:` inside an `animate` block, as `(line, value)`.
    ///
    /// The block is walked by braces, not by line, so the multi-line form
    /// (`animate opacity {` / `duration: 120ms;` / `}`) is caught as well as the
    /// one-line form the components use. A line whose `animate` sits inside a
    /// comment is skipped: mentioning the keyword is not animating anything.
    fn animate_durations(source: &str) -> Vec<(usize, String)> {
        let mut found = Vec::new();
        for (start, _) in source.match_indices("animate ") {
            let line_start = source[..start].rfind('\n').map_or(0, |at| at + 1);
            let line_end = source[start..]
                .find('\n')
                .map_or(source.len(), |at| start + at);
            if !source[line_start..line_end].contains("animate ")
                || source[line_start..start].contains("//")
            {
                continue;
            }
            let Some(open) = source[start..].find('{').map(|at| start + at) else {
                continue;
            };
            let mut depth = 0usize;
            let mut end = source.len();
            for (offset, ch) in source[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + offset;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let mut cursor = 0usize;
            for text in source[open..end].split('\n') {
                let code = text.split("//").next().unwrap_or("");
                if let Some(at) = code.find("duration:") {
                    let value: String = code[at + "duration:".len()..]
                        .chars()
                        .take_while(|c| *c != ';' && *c != '}')
                        .collect();
                    let line = source[..open + cursor + at].matches('\n').count() + 1;
                    found.push((line, value.trim().to_string()));
                }
                cursor += text.len() + 1;
            }
        }
        found
    }

    // -----------------------------------------------------------------------
    // Text alignment (round ten)
    // -----------------------------------------------------------------------

    /// One element block in a .slint file, by the byte range of its body.
    struct ElementBlock {
        name: String,
        start: usize,
        end: usize,
        body: String,
    }

    /// Every "Name { ... }" block in a source, brace-matched and string/comment aware.
    ///
    /// The alignment rule below needs to know which *layout* a Text sits in, and that
    /// is a nesting question the compiler will not answer for us. The scan records the
    /// element name that precedes each opening brace and the byte range it owns, which
    /// is all "is this text a direct child of a horizontal layout" needs.
    fn element_blocks(source: &str) -> Vec<ElementBlock> {
        let bytes = source.as_bytes();
        let mut blocks = Vec::new();
        let mut stack: Vec<(String, usize)> = Vec::new();
        let mut cursor = 0usize;
        while cursor < bytes.len() {
            let ch = bytes[cursor] as char;
            // Skip line comments: prose in this project mentions braces.
            if ch == '/' && bytes.get(cursor + 1) == Some(&b'/') {
                while cursor < bytes.len() && bytes[cursor] != b'\n' {
                    cursor += 1;
                }
                continue;
            }
            // Skip string literals: a placeholder or a tip may carry a brace.
            if ch == '"' {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor] != b'"' {
                    if bytes[cursor] == b'\\' {
                        cursor += 1;
                    }
                    cursor += 1;
                }
                cursor += 1;
                continue;
            }
            if ch == '{' {
                // The name is the identifier before the brace, so "if c : Text {" and
                // a plain "Text {" both report Text.
                let mut back = cursor;
                while back > 0 && (bytes[back - 1] as char).is_whitespace() {
                    back -= 1;
                }
                let mut start = back;
                while start > 0 {
                    let c = bytes[start - 1] as char;
                    if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
                        start -= 1;
                    } else {
                        break;
                    }
                }
                stack.push((source[start..back].to_string(), cursor));
            } else if ch == '}' {
                if let Some((name, open)) = stack.pop() {
                    blocks.push(ElementBlock {
                        name,
                        start: open,
                        end: cursor,
                        body: source[open..cursor].to_string(),
                    });
                }
            }
            cursor += 1;
        }
        blocks.sort_by_key(|block| block.start);
        blocks
    }

    /// The innermost block that encloses the given one.
    fn enclosing_block<'a>(
        blocks: &'a [ElementBlock],
        block: &ElementBlock,
    ) -> Option<&'a ElementBlock> {
        blocks
            .iter()
            .filter(|candidate| candidate.start < block.start && candidate.end > block.end)
            .min_by_key(|candidate| candidate.end - candidate.start)
    }

    /// Rule: a Text that is a direct child of a HorizontalLayout states its
    /// cross-axis alignment.
    ///
    /// The icon layer already has this rule applied to glyphs (a fixed-height child of
    /// a horizontal layout is aligned to the start of the cross axis, not centred);
    /// this is the same fact about text. A Text that is handed a height - which every
    /// caption inside a 44px card footer is - puts its first line at the top of that
    /// box, so the label sits 3-6px above the count beside it and the row reads as
    /// misaligned. No offset fixes it: the box has to say where the line goes.
    ///
    /// "vertical-alignment: center" answers one line, "top" answers a wrapped
    /// paragraph, and an explicit "y:" answers both by positioning the box. Text in a
    /// VerticalLayout is exempt by construction: there the vertical axis is the
    /// layout main axis, so the layout places the box.
    #[test]
    fn text_in_a_horizontal_row_states_its_cross_axis_alignment() {
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let source = read(&path);
            let blocks = element_blocks(&source);
            for block in blocks.iter().filter(|block| block.name == "Text") {
                let Some(parent) = enclosing_block(&blocks, block) else {
                    continue;
                };
                if parent.name != "HorizontalLayout" {
                    continue;
                }
                checked += 1;
                let declares = block.body.contains("vertical-alignment")
                    || block.body.lines().any(|line| {
                        let code = line.split("//").next().unwrap_or("");
                        code.trim_start().starts_with("y:")
                    });
                if !declares {
                    let line = source[..block.start].lines().count() + 1;
                    offenders.push(format!("{}:{}", path.display(), line));
                }
            }
        }
        assert!(
            checked >= 4,
            "expected to inspect several Text nodes directly inside a HorizontalLayout, found {checked}; the scan is broken, not the UI"
        );
        assert!(
            offenders.is_empty(),
            "these Text nodes sit directly in a HorizontalLayout without stating where their line goes (add vertical-alignment: center for one line, top for a paragraph, or use Label): {offenders:?}"
        );
    }

    /// Rule: the reader bounds are one pair of numbers in both languages.
    ///
    /// Theme declares the clamp bounds the settings sliders show;
    /// crates/pixiv-novel/src/prefs.rs declares the ones that actually clamp the
    /// stored value. Two copies of one range cannot be caught by the Slint compiler,
    /// because the two live in different languages, so the gate reads both texts.
    #[test]
    fn reader_bounds_match_the_preference_model() {
        let theme = read(&ui_dir().join("theme.slint"));
        let prefs = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/pixiv-novel/src/prefs.rs"),
        )
        .expect("the reader preference model must exist");
        // Only the declaration line: the name also appears in the clamp expression,
        // where the next number belongs to a different constant.
        let token = |source: &str, name: &str| -> f32 {
            let line = source
                .lines()
                .find(|line| {
                    let code = line.split("//").next().unwrap_or("");
                    code.contains(name) && (code.contains('=') || code.contains(':'))
                })
                .unwrap_or_else(|| panic!("missing a declaration of {name}"));
            // The *last* number on the declaration line, not the first: the type is
            // "f32", whose digits would otherwise be read as the value (that is
            // exactly what this rule reported before it was fixed: 14.0 read as 32).
            let mut runs: Vec<String> = Vec::new();
            let mut current = String::new();
            for ch in line.chars() {
                if ch.is_ascii_digit() || (ch == '.' && !current.is_empty()) {
                    current.push(ch);
                } else if !current.is_empty() {
                    runs.push(std::mem::take(&mut current));
                }
            }
            if !current.is_empty() {
                runs.push(current);
            }
            let digits = runs
                .pop()
                .unwrap_or_else(|| panic!("no number on the declaration of {name}"));
            digits.parse().unwrap_or_else(|e| panic!("{name}: {e}"))
        };
        let pairs = [
            ("reader-font-min", "MIN_FONT_SIZE"),
            ("reader-font-max", "MAX_FONT_SIZE"),
            ("reader-measure-min", "MIN_MAX_WIDTH"),
            ("reader-measure-max", "MAX_MAX_WIDTH"),
        ];
        let mut mismatched = Vec::new();
        for (theme_name, prefs_name) in pairs {
            let shown = token(&theme, theme_name);
            let enforced = token(&prefs, prefs_name);
            if (shown - enforced).abs() > f32::EPSILON {
                mismatched.push(format!("{theme_name}={shown} but {prefs_name}={enforced}"));
            }
        }
        assert!(
            mismatched.is_empty(),
            "the reader sliders and the model that clamps them disagree: {mismatched:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Round eleven (docs/OPTIMIZATION-PLAN-R11.md §6, track A)
    // -----------------------------------------------------------------------

    /// The code on a block's opening line, before its `{`.
    ///
    /// `ElementBlock` records where a block starts, not what announced it, and the two
    /// rules below are about *conditional* surfaces (`if root.x : Rectangle {`).
    fn opener(source: &str, block: &ElementBlock) -> String {
        let start = source[..block.start].rfind('\n').map_or(0, |at| at + 1);
        source[start..block.start]
            .split("//")
            .next()
            .unwrap_or("")
            .to_string()
    }

    /// G29 — a state overlay is declared after the content it covers.
    ///
    /// Slint hit-tests in reverse declaration order, so a full-surface overlay declared
    /// *before* the thing it covers loses every click to it: the panel looks open and is
    /// pointer-dead. The rule asks about the shape that makes it an overlay — a
    /// conditional block that claims the whole window (`root.width` and `root.height`)
    /// and carries a hit surface — and then insists that no bare hit surface is declared
    /// after it outside a conditional of its own.
    #[test]
    fn a_state_overlay_is_declared_after_the_content_it_covers() {
        let mut offenders = Vec::new();
        let mut overlays = 0;
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            let blocks = element_blocks(&source);
            for block in &blocks {
                if !opener(&source, block).contains("if ") {
                    continue;
                }
                let own = own_lines(&block.body);
                let covers = (own.contains("width: root.width") || own.contains("width: 100%"))
                    && (own.contains("height: root.height") || own.contains("height: 100%"));
                if !covers || !own.contains("TouchArea") {
                    continue;
                }
                overlays += 1;
                let overlay_line = source[..block.start].lines().count() + 1;
                for later in blocks.iter().filter(|later| later.start > block.start) {
                    if later.name != "TouchArea" {
                        continue;
                    }
                    let mut conditional = false;
                    let mut ancestor = enclosing_block(&blocks, later);
                    while let Some(parent) = ancestor {
                        if opener(&source, parent).contains("if ") {
                            conditional = true;
                            break;
                        }
                        ancestor = enclosing_block(&blocks, parent);
                    }
                    if conditional {
                        continue;
                    }
                    let line = source[..later.start].lines().count() + 1;
                    offenders.push(format!(
                        "{name}:{line}: a hit surface is declared after the overlay at line {overlay_line}"
                    ));
                }
            }
        }
        assert!(
            overlays >= 3,
            "expected the shell and the reader to carry full-surface overlays, found {overlays}"
        );
        assert!(
            offenders.is_empty(),
            "an overlay declared before the content it covers receives no clicks. Move it \
last in its component:\n{}",
            offenders.join("\n")
        );
    }

    /// G30 — every hit surface that takes a click declares a role *and* a name.
    ///
    /// Round ten asked the question of a fixed list of ten components, which checks the
    /// primitives and nothing that uses them. This asks it of every `TouchArea` with a
    /// `clicked` handler: either the surface names itself, or the cell it lives in does
    /// (the shape every working primitive already uses — role and label on the root, the
    /// pointer target last). A dismiss-only backdrop is exempt and must name itself
    /// `*-scrim`: Escape is its keyboard path, and a screen reader narrating "background"
    /// between every panel is worse than silence.
    #[test]
    fn every_hit_surface_that_takes_a_click_declares_a_role_and_a_name() {
        // Files another track owns this round (plan §7): their hit surfaces are that
        // track's debt. The sample is every file this track owns, plus the shared
        // primitives — which is where the negative experiment lands.
        const FILED_WITH_ANOTHER_TRACK: [&str; 5] = [
            "detail.slint",
            "downloads.slint",
            "novel.slint",
            "novels.slint",
            "profile.slint",
        ];
        let mut offenders = Vec::new();
        let mut checked = 0;
        let mut scrims = 0;
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if FILED_WITH_ANOTHER_TRACK.contains(&name.as_str()) {
                continue;
            }
            let source = read(&path);
            let blocks = element_blocks(&source);
            for block in blocks.iter().filter(|block| block.name == "TouchArea") {
                if !own_lines(&block.body).contains("clicked") {
                    continue;
                }
                if opener(&source, block).contains("scrim") {
                    scrims += 1;
                    continue;
                }
                checked += 1;
                // The surface names itself, or the cell it lives in does, or the
                // component does — the three shapes every working primitive uses. The
                // walk goes all the way up because a `for` delegate's parent is the
                // layout, not the component that gave the row its role.
                let mut covered = block.body.contains("accessible-role")
                    && block.body.contains("accessible-label");
                let mut ancestor = enclosing_block(&blocks, block);
                while let Some(parent) = ancestor {
                    if parent.body.contains("accessible-role")
                        && parent.body.contains("accessible-label")
                    {
                        covered = true;
                        break;
                    }
                    ancestor = enclosing_block(&blocks, parent);
                }
                if !covered {
                    let line = source[..block.start].lines().count() + 1;
                    offenders.push(format!("{name}:{line}"));
                }
            }
        }
        assert!(
            checked >= 10 && scrims >= 1,
            "expected many named hit surfaces and at least one scrim; found {checked} and {scrims}"
        );
        assert!(
            offenders.is_empty(),
            "these hit surfaces take a click and expose neither a role nor a name:\n{}",
            offenders.join("\n")
        );
    }

    /// G31 — every shared control is created with its accessible name.
    ///
    /// The role rules check the primitive; nothing checked the *call site*, which is where
    /// the name actually comes from: a `Switch` created without `label:` is a switch a
    /// screen reader cannot describe, and an `IconButton` without `tip:` is a glyph with
    /// no meaning. One property per primitive — the one that carries its name.
    #[test]
    fn every_shared_control_is_created_with_its_accessible_name() {
        const NAMED: [(&str, &str); 7] = [
            ("AppButton", "label"),
            ("IconButton", "tip"),
            ("TextField", "placeholder"),
            ("ToggleSwitch", "label"),
            ("SegmentedControl", "label"),
            ("TagChip", "label"),
            ("NavItem", "label"),
        ];
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            let blocks = element_blocks(&source);
            for block in &blocks {
                let Some((_, prop)) = NAMED.iter().find(|(control, _)| *control == block.name)
                else {
                    continue;
                };
                if opener(&source, block).contains("component ") {
                    continue;
                }
                checked += 1;
                if !own_lines(&block.body).contains(&format!("{prop}:")) {
                    let line = source[..block.start].lines().count() + 1;
                    offenders.push(format!("{name}:{line}: {} without {prop}:", block.name));
                }
            }
        }
        assert!(
            checked >= 40,
            "expected to inspect every shared control instance; found {checked}"
        );
        assert!(
            offenders.is_empty(),
            "a control created without its name is a control a screen reader cannot describe:\n{}",
            offenders.join("\n")
        );
    }

    /// G31 — radii, type sizes and the theme's spacing steps are theme-only, everywhere
    /// but the theme itself.
    ///
    /// Round ten asked this question of colours in page files. The defect class is the
    /// same one dimension over: a 12px radius, a 19px font size and a 4px gutter are all
    /// numbers two files must agree on, and a literal is a number two files will disagree
    /// on. The colour half is `pages_take_colours_from_the_theme`, which already reads
    /// every file; this is the radius, type, spacing and control-size half (U32/U35 end
    /// green under it).
    #[test]
    fn radii_type_sizes_and_spacing_steps_come_from_the_theme() {
        // (file, property, value) that belongs to a file another track owns this round
        // (plan §7). Named rather than skipped: profile.slint's two 4px spacings are
        // `Theme.space-1`, and the file is track D's.
        const FILED_WITH_ANOTHER_TRACK: [(&str, &str, f32); 1] =
            [("profile.slint", "spacing", 4.0)];
        let theme = read(&ui_dir().join("theme.slint"));
        let theme_value = |name: &str| -> Option<f32> {
            theme.lines().find_map(|line| {
                let code = line.split("//").next().unwrap_or("").trim();
                let rest = code.strip_prefix(&format!("out property <length> {name}: "))?;
                rest.trim_end_matches(';')
                    .trim()
                    .strip_suffix("px")?
                    .parse()
                    .ok()
            })
        };
        let mut steps = Vec::new();
        for step in 1..=8 {
            if let Some(value) = theme_value(&format!("space-{step}")) {
                steps.push(value);
            }
        }
        let mut icon_sizes = Vec::new();
        for name in ["icon-size", "icon-size-lg"] {
            if let Some(value) = theme_value(name) {
                icon_sizes.push(value);
            }
        }
        let control = theme_value("control-height").expect("the control height is a token");
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            if path.file_name().and_then(|n| n.to_str()) == Some("theme.slint") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("").trim();
                let mut offender = |what: String| {
                    offenders.push(format!("{name}:{}: {what}", index + 1));
                };
                // Every one of these properties is *inspected*; the count is the rule's
                // sample, so a tree with nothing left to report still proves it looked.
                for prop in ["border-radius", "font-size", "spacing", "padding", "size"] {
                    if code.starts_with(&format!("{prop}:")) {
                        checked += 1;
                    }
                }
                if let Some(radius) = bare_px(code, "border-radius") {
                    offender(format!("border-radius: {radius}px"));
                }
                if let Some(size) = bare_px(code, "font-size") {
                    offender(format!("font-size: {size}px"));
                }
                let spacing = bare_px(code, "spacing")
                    .or_else(|| bare_px(code, "padding"))
                    .or_else(|| bare_px(code, "padding-left"))
                    .or_else(|| bare_px(code, "padding-right"))
                    .or_else(|| bare_px(code, "padding-top"))
                    .or_else(|| bare_px(code, "padding-bottom"));
                if let Some(value) = spacing {
                    if value > 0.0
                        && steps
                            .iter()
                            .any(|step| (*step - value).abs() < f32::EPSILON)
                    {
                        let filed = FILED_WITH_ANOTHER_TRACK
                            .iter()
                            .any(|(file, _, v)| *file == name && (*v - value).abs() < f32::EPSILON);
                        if !filed {
                            offender(format!("a {value}px spacing step is Theme.space-*"));
                        }
                    }
                }
                if let Some(value) = bare_px(code, "size") {
                    if icon_sizes
                        .iter()
                        .any(|icon| (*icon - value).abs() < f32::EPSILON)
                    {
                        offender(format!("size: {value}px is Theme.icon-size*"));
                    }
                }
            }
            // U34: a button shorter than the control height is a compact button, and a
            // compact button that does not say so paints outside the row it was put in.
            let blocks = element_blocks(&source);
            for block in blocks.iter().filter(|block| block.name == "AppButton") {
                if opener(&source, block).contains("component ") {
                    continue;
                }
                let own = own_lines(&block.body);
                let Some(height) = bare_px(&own, "row-height") else {
                    continue;
                };
                if height <= 0.0 || height >= control {
                    continue;
                }
                checked += 1;
                if !own.contains("compact: true") {
                    let line = source[..block.start].lines().count() + 1;
                    offenders.push(format!(
                        "{name}:{line}: a {height}px AppButton in a {control}px world needs compact: true"
                    ));
                }
            }
        }
        assert!(
            checked >= 10,
            "expected to inspect literals and short buttons; found {checked}"
        );
        assert!(
            offenders.is_empty(),
            "radii, font sizes and spacing steps are theme tokens, not literals:\n{}",
            offenders.join("\n")
        );
    }

    /// U32 — the theme's card-width band is the one `src/layout.rs` balances with.
    ///
    /// `Theme.card-min-width` / `card-max-width` are the only numbers that exist twice in
    /// two languages: Slint draws the card, Rust decides how many columns fit. Nothing
    /// tied them together, so the balancer could clamp a card to a band the grid would
    /// never honour. The shape is `reader_bounds_match_the_preference_model`'s: read the
    /// declaration out of both texts and compare.
    #[test]
    fn grid_widths_match_the_layout_model() {
        let theme = read(&ui_dir().join("theme.slint"));
        let layout =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/layout.rs"))
                .expect("the layout model must exist");
        let last_number = |line: &str| -> f32 {
            let mut current = String::new();
            let mut runs: Vec<String> = Vec::new();
            for ch in line.chars() {
                if ch.is_ascii_digit() || (ch == '.' && !current.is_empty()) {
                    current.push(ch);
                } else if !current.is_empty() {
                    runs.push(std::mem::take(&mut current));
                }
            }
            if !current.is_empty() {
                runs.push(current);
            }
            runs.pop()
                .unwrap_or_else(|| panic!("no number on {line:?}"))
                .parse()
                .unwrap_or_else(|e| panic!("{line:?}: {e}"))
        };
        let pairs = [
            ("card-min-width", "CARD_MIN_WIDTH"),
            ("card-max-width", "CARD_MAX_WIDTH"),
        ];
        let mut mismatched = Vec::new();
        for (token, constant) in pairs {
            let shown = last_number(
                theme
                    .lines()
                    .find(|line| line.contains(&format!(" {token}: ")))
                    .unwrap_or_else(|| panic!("theme.slint must declare {token}")),
            );
            let enforced = last_number(
                layout
                    .lines()
                    .find(|line| line.contains(&format!("const {constant}: f32")))
                    .unwrap_or_else(|| panic!("layout.rs must declare {constant}")),
            );
            if (shown - enforced).abs() > f32::EPSILON {
                mismatched.push(format!(
                    "Theme.{token}={shown} but layout::{constant}={enforced}"
                ));
            }
        }
        assert!(
            mismatched.is_empty(),
            "the theme's card widths and the Rust balancer disagree: {mismatched:?}"
        );
    }

    /// The value of a bare `prop: <n>px` line in a block, if there is one.
    ///
    /// "Bare" is the whole point of the literal rules below: `height: 100%` fills the
    /// cross axis and needs no alignment, and `Theme.space-2` is a token, not a
    /// literal. Only a plain number with the `px` unit is a literal the theme owns.
    fn bare_px(body: &str, prop: &str) -> Option<f32> {
        for line in body.lines() {
            let code = line.split("//").next().unwrap_or("").trim();
            let Some(rest) = code.strip_prefix(prop) else {
                continue;
            };
            let Some(value) = rest.strip_prefix(':') else {
                continue;
            };
            let value = value.trim().trim_end_matches(';').trim();
            let Some(digits) = value.strip_suffix("px") else {
                continue;
            };
            if let Ok(number) = digits.trim().parse::<f32>() {
                return Some(number);
            }
        }
        None
    }

    /// The lines a block owns, i.e. its own declarations and not a child's.
    ///
    /// `ElementBlock.body` holds the whole sub-tree, so `body.contains("width: 100%")`
    /// answers a question about the children as often as about the element — the first
    /// draft of the two rules below flagged six nested layouts that "claimed the page"
    /// because a grandchild did. Depth-aware filtering is what makes "this box claims
    /// the page" a statement about the box.
    fn own_lines(body: &str) -> String {
        // A block's body starts at its own `{` and stops before its matching `}`, so
        // the opening brace is stripped once and the depth then starts at zero.
        let inner = body.strip_prefix('{').unwrap_or(body);
        let mut out = String::new();
        let mut depth = 0i32;
        for line in inner.lines() {
            let code = line.split("//").next().unwrap_or("");
            if depth == 0 {
                out.push_str(code);
                out.push('\n');
            }
            depth += code.matches('{').count() as i32;
            depth -= code.matches('}').count() as i32;
        }
        out
    }

    /// True when a block body declares `clip: true`.
    fn clips(body: &str) -> bool {
        body.lines().any(|line| {
            let code = line.split("//").next().unwrap_or("").trim();
            code == "clip: true;" || code == "clip: true"
        })
    }

    /// G26 — a page-level clipped body must be reachable through a `ScrollView`.
    ///
    /// The class this prevents is the one four earlier rounds kept re-introducing: a
    /// body that clips its overflow instead of scrolling it, so the content below the
    /// fold exists and cannot be reached. The rule only asks about a box that claims the
    /// page on both axes (`100%`, `root.width`, `root.height` or `vertical-stretch: 1`)
    /// and is not inside a scroller: a 48px chip strip may clip, a page body may not.
    #[test]
    fn a_clipped_page_body_is_reachable_through_a_scroll_view() {
        const SCROLLERS: [&str; 3] = ["ScrollView", "PageScroll", "Flickable"];
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            if !path.starts_with(ui_dir().join("pages")) {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            let blocks = element_blocks(&source);
            for block in &blocks {
                let own = own_lines(&block.body);
                if !clips(&own) || SCROLLERS.contains(&block.name.as_str()) {
                    continue;
                }
                checked += 1;
                let claims_page = (own.contains("width: 100%")
                    || own.contains("width: root.width"))
                    && (own.contains("height: 100%")
                        || own.contains("height: root.height")
                        || own.contains("vertical-stretch: 1"));
                if !claims_page {
                    continue;
                }
                let mut ancestor = enclosing_block(&blocks, block);
                let mut scrollable = false;
                while let Some(parent) = ancestor {
                    if SCROLLERS.contains(&parent.name.as_str()) {
                        scrollable = true;
                        break;
                    }
                    ancestor = enclosing_block(&blocks, parent);
                }
                if scrollable {
                    continue;
                }
                let line = source[..block.start].lines().count() + 1;
                offenders.push(format!("{name}:{line}: {} clips the page box", block.name));
            }
        }
        assert!(
            checked >= 4,
            "expected several clipped boxes in ui/pages; found {checked}, so the scan is broken"
        );
        assert!(
            offenders.is_empty(),
            "a page body that clips instead of scrolling hides its own content. Put it in a \
             ScrollView (or PageScroll):\n{}",
            offenders.join("\n")
        );
    }

    /// Kebab-case to snake_case: `shell-height` is `get_shell_height` in Rust.
    fn snake(name: &str) -> String {
        name.replace('-', "_")
    }

    /// How many times `name` is *used* in `source`, ignoring the declaration line,
    /// binding targets and empty handler bodies.
    ///
    /// A binding target (`name: value`) writes the declaration and is not a reader; an
    /// empty body (`name => { }`) is not an invoker either — that is the shape that let
    /// `UserCard.toggle-follow` live for a whole round with two call sites that did
    /// nothing. Everything else counts.
    fn uses(source: &str, name: &str, skip_line: usize) -> usize {
        let mut count = 0;
        for (index, line) in source.lines().enumerate() {
            if index + 1 == skip_line {
                continue;
            }
            let code = line.split("//").next().unwrap_or("");
            let indent = code.len() - code.trim_start().len();
            for (at, _) in code.match_indices(name) {
                let before = code[..at].chars().next_back();
                if matches!(before, Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    continue;
                }
                let after = &code[at + name.len()..];
                match after.chars().next() {
                    Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_' => continue,
                    _ => {}
                }
                if at == indent && after.trim_start().starts_with(':') {
                    continue;
                }
                if after.trim_start().starts_with("=> { }")
                    || after.trim_start().starts_with("=> {}")
                {
                    continue;
                }
                count += 1;
            }
        }
        count
    }

    /// G28 + G39 — a declaration has a reader: a `property` has a read site and a
    /// `callback` has an invoker (and a `public function` a caller).
    ///
    /// A declaration nobody reads is silent: Slint compiles it, the app runs, and the
    /// surface it was declared for is either missing or was removed a round ago. Measured
    /// on this tree the rule named `open-card-author` and `novel-detail-back` on the app
    /// window, `UserCard.toggle-follow` (declared, then bound twice to an empty body),
    /// `TagChip.take-focus` (a focus entry with no caller), `InfoBar.dismissible` and
    /// eleven theme tokens. The tree is clean now, so the rule lands green.
    ///
    /// Three samples, because "a reader" means three different things:
    ///
    /// * a component declaration — the reader is a `.slint` file that uses it;
    /// * a `Theme` token — the reader is a `.slint` file that reads `Theme.<name>`, or the
    ///   Rust half of the same contract (the mirror tests read the name on both sides);
    /// * a declaration on the app window — the reader is the UI, or the host: these are
    ///   the Rust seam, so `get_`/`set_`/`on_` in `src/` counts.
    ///
    /// The check is name-based, and one consequence is worth writing down: a declaration
    /// whose *name* is also declared by another component looks alive, because the scan
    /// cannot tell `UserCard.toggle-follow` from `ProfilePage.toggle-follow`. Measured
    /// while proving the rule: re-adding `UserCard.toggle-follow` passes, because
    /// `ProfilePage` uses a callback of the same name. That is the price of a text rule;
    /// the deletions below were confirmed one at a time against a name that is unique
    /// (`InfoBar.dismissible`).
    #[test]
    fn every_declaration_has_a_reader() {
        // Declarations another track owns this round (plan §7). Named, with a reason,
        // rather than silently skipped: a dead declaration that this rule cannot reach
        // is still a dead declaration.
        const FILED_WITH_ANOTHER_TRACK: [(&str, &str); 1] = [("profile.slint", "webpage")];
        // Theme tokens whose reader is the Rust half of a two-language contract.
        const THEME_READERS_ELSEWHERE: [(&str, &str); 8] = [
            ("reader-font-min", "src/uiaudit.rs pins it against prefs.rs"),
            ("reader-font-max", "src/uiaudit.rs pins it against prefs.rs"),
            (
                "reader-measure-min",
                "src/uiaudit.rs pins it against prefs.rs",
            ),
            (
                "reader-measure-max",
                "src/uiaudit.rs pins it against prefs.rs",
            ),
            (
                "card-min-width",
                "grid_widths_match_the_layout_model pins it",
            ),
            (
                "card-max-width",
                "grid_widths_match_the_layout_model pins it",
            ),
            // The brand colour may be a decorative fill and nothing may paint text or
            // glyphs with it, so `accent_is_decoration_only` forbids a reader on purpose.
            ("accent", "accent_is_decoration_only forbids a reader"),
            // A frozen seam for track D (plan §1/§4 F4): a 3px progress rule under a
            // partly-read row. It has no reader *until* F4 lands, and it may not be
            // invented at the call site then.
            (
                "progress-rule-height",
                "frozen seam for track D, plan §4 F4",
            ),
        ];
        let files: Vec<(String, String)> = page_files()
            .into_iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                (name, read(&path))
            })
            .collect();
        let mut rust = String::new();
        for path in rust_files() {
            rust.push_str(&read(&path));
            rust.push('\n');
        }
        let mut offenders = Vec::new();
        let mut checked = 0;
        for (name, source) in &files {
            if name == "theme.slint" {
                continue;
            }
            let is_window = name == "main.slint";
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                let trimmed = code.trim_start();
                let Some((kind, declared)) = declared_name(trimmed) else {
                    continue;
                };
                if FILED_WITH_ANOTHER_TRACK
                    .iter()
                    .any(|(file, field)| file == name && *field == declared)
                {
                    continue;
                }
                checked += 1;
                let used_here: usize = files
                    .iter()
                    .map(|(other_name, other)| {
                        let skip = if other_name == name { index + 1 } else { 0 };
                        uses(other, &declared, skip)
                    })
                    .sum();
                let used_rust = if is_window && used_here == 0 {
                    let suffix = snake(&declared);
                    rust.contains(&format!("get_{suffix}"))
                        || rust.contains(&format!("set_{suffix}"))
                        || rust.contains(&format!("on_{suffix}"))
                } else {
                    false
                };
                if used_here == 0 && !used_rust {
                    offenders.push(format!("{name}:{}: {kind} {declared}", index + 1));
                }
            }
        }
        // (b) the Theme global: every token has a reader, here or in Rust.
        let theme = read(&ui_dir().join("theme.slint"));
        // Only the `Theme` global: `Type` below it is an int ramp read as `Type.semibold`,
        // and a name-based check over the whole file would ask the wrong question.
        let theme = theme
            .split("export global Type")
            .next()
            .unwrap_or(theme.as_str())
            .to_string();
        let readers = files
            .iter()
            .filter(|(name, _)| name != "theme.slint")
            .map(|(_, source)| source.as_str())
            .collect::<Vec<_>>();
        for (index, line) in theme.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let Some(rest) = code.trim_start().strip_prefix("out property <") else {
                continue;
            };
            let Some(after) = rest.split('>').nth(1) else {
                continue;
            };
            let declared: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if declared.is_empty()
                || THEME_READERS_ELSEWHERE
                    .iter()
                    .any(|(name, _reason)| *name == declared)
            {
                continue;
            }
            checked += 1;
            let needle = format!("Theme.{declared}");
            let read_somewhere = readers.iter().any(|source| {
                source.lines().any(|line| {
                    let code = line.split("//").next().unwrap_or("");
                    code.match_indices(&needle).any(|(at, _)| {
                        !matches!(code[at + needle.len()..].chars().next(),
                            Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    })
                })
            });
            if !read_somewhere {
                offenders.push(format!(
                    "theme.slint:{}: token {declared} has no reader",
                    index + 1
                ));
            }
        }
        assert!(
            checked > 200,
            "expected to inspect the whole declaration surface; found {checked}"
        );
        assert!(
            offenders.is_empty(),
            "these declarations have no reader — delete them or wire them:\n{}",
            offenders.join("\n")
        );
    }

    /// The declaration on a trimmed line, as (kind, name).
    fn declared_name(line: &str) -> Option<(&'static str, String)> {
        if line.contains("component ") || line.starts_with("export ") || line.starts_with("import ")
        {
            return None;
        }
        let rest = line
            .strip_prefix("in-out property <")
            .or_else(|| line.strip_prefix("in property <"))
            .or_else(|| line.strip_prefix("out property <"))
            .or_else(|| line.strip_prefix("private property <"))
            // A component-local `property <T> name` is a declaration like any other: the
            // plan's own experiment plants one, and a local nobody reads is the same
            // silent dead weight.
            .or_else(|| line.strip_prefix("property <"));
        if let Some(rest) = rest {
            let name: String = rest
                .split('>')
                .nth(1)?
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if name.is_empty() {
                return None;
            }
            return Some(("property", name));
        }
        for prefix in ["callback ", "public function ", "function "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                if name.is_empty() {
                    return None;
                }
                let kind = if prefix == "callback " {
                    "callback"
                } else {
                    "function"
                };
                return Some((kind, name));
            }
        }
        None
    }

    /// Every `.rs` file under `src/`, for the rules that have to read the Rust half of a
    /// contract (the theme is two languages).
    fn rust_files() -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_with_extension(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            "rs",
            &mut files,
        );
        files
    }
    /// G27 — widened from `uiaudit.rs:1514`: *every* fixed-size child of a horizontal row
    /// states its cross-axis alignment, not only `Text`.
    ///
    /// A child of a `HorizontalLayout` that states its own height is placed at the *start*
    /// of the cross axis, which is why the icon layer needed `IconPresenter`, the text rule
    /// needed `vertical-alignment`, and a button needs `row-height` or `y:`. The old rule
    /// asked the question of `Text` only; a `Rectangle` with a literal height and no `y:`
    /// has exactly the same bug and had no such rule, so the widening is the change and the
    /// sample grows with it.
    #[test]
    fn a_fixed_size_child_of_a_horizontal_row_states_its_cross_axis_alignment() {
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let source = read(&path);
            let blocks = element_blocks(&source);
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            for block in &blocks {
                let Some(parent) = enclosing_block(&blocks, block) else {
                    continue;
                };
                if parent.name != "HorizontalLayout" {
                    continue;
                }
                let Some(height) = bare_px(&own_lines(&block.body), "height") else {
                    continue;
                };
                if height <= 0.0 {
                    continue;
                }
                checked += 1;
                let aligned = block.body.contains("vertical-alignment")
                    || block.body.lines().any(|line| {
                        let code = line.split("//").next().unwrap_or("").trim_start();
                        code.starts_with("y:")
                    });
                if !aligned {
                    let line = source[..block.start].lines().count() + 1;
                    offenders.push(format!(
                        "{name}:{line}: {} (height {height}px, no y: and no vertical-alignment)",
                        block.name
                    ));
                }
            }
        }
        assert!(
            checked >= 4,
            "expected to inspect several fixed-size children of a HorizontalLayout, found \
             {checked}; the scan is broken, not the UI"
        );
        assert!(
            offenders.is_empty(),
            "a fixed-height child of a HorizontalLayout is aligned to the start of the cross \
             axis. State where it goes (y:, row-height: or vertical-alignment:):\n{}",
            offenders.join("\n")
        );
    }
    // -----------------------------------------------------------------------
    // Round twelve (docs/OPTIMIZATION-PLAN-R12.md §9, track A)
    // -----------------------------------------------------------------------

    /// Names of the components whose body declares a pointer callback.
    ///
    /// Read out of the tree rather than listed here: a shared control becomes a pointer
    /// target the moment it declares `clicked`, and a hand-written list would have gone
    /// stale the first time one was added — which is the failure mode this round keeps
    /// finding.
    fn pointer_targets() -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for path in page_files() {
            let source = read(&path);
            for (index, line) in source.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                let Some(at) = code.find("component ") else {
                    continue;
                };
                let rest = &code[at + "component ".len()..];
                let ident: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                    .collect();
                if ident.is_empty() {
                    continue;
                }
                let body = component_body(&source, index);
                if body.contains("callback clicked") {
                    out.insert(ident);
                }
            }
        }
        out
    }

    /// True when a block's own lines carry a `clicked =>` handler.
    ///
    /// `double-clicked` is a different event and must not count: the image viewer's pan
    /// surface answers it and takes no single press, which is exactly the surface a
    /// "has a click of its own" test would have waved through.
    fn owns_click(own: &str) -> bool {
        own.match_indices("clicked").any(|(at, _)| {
            let before = own[..at].chars().next_back();
            if matches!(before, Some(c) if c.is_ascii_alphanumeric() || c == '-') {
                return false;
            }
            own[at + "clicked".len()..].trim_start().starts_with("=>")
        })
    }

    /// The body of the handler that begins at `at`: everything between its opening
    /// brace and that brace's partner.
    ///
    /// `moved => { ... }` is the only handler shape these rules ask about, and the body
    /// has to be walked by braces rather than by line: the slider's is three lines and the
    /// viewer's is six, and a line-based read would decide the question on whichever
    /// fragment happened to come first.
    fn handler_body(source: &str, at: usize) -> String {
        let Some(open) = source[at..].find('{').map(|offset| at + offset) else {
            return String::new();
        };
        let mut depth = 0usize;
        for (offset, ch) in source[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return source[open + 1..open + offset].to_string();
                    }
                }
                _ => {}
            }
        }
        String::new()
    }

    /// True when the body assigns to a property: `root.x = …`, `self.x = …`,
    /// `parent.x = …`. A comparison is not an assignment.
    fn assigns_a_property(body: &str) -> bool {
        body.lines().any(|line| {
            let code = line.split("//").next().unwrap_or("").trim();
            ["root.", "self.", "parent."].iter().any(|prefix| {
                let Some(rest) = code.strip_prefix(prefix) else {
                    return false;
                };
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                let after = rest[name.len()..].trim_start();
                !name.is_empty() && after.starts_with('=') && !after.starts_with("==")
            })
        })
    }

    /// G46 — a `moved =>` handler that writes a value must guard on `pressed`.
    ///
    /// `moved` fires whenever the pointer moves anywhere over the area — a hover, not a
    /// drag. A handler that assigns from `self.mouse-x` therefore rewrites its property on
    /// every pointer sweep across it: the novel reader's seven preference sliders were
    /// driven by `moved` alone, so sweeping the pointer down the settings panel changed
    /// font size, line height, letter spacing, paragraph spacing, indent, margins and
    /// measure — seven persisted settings, no click anywhere. The image viewer next door
    /// has always had the guard, which is why the same mistake in one file and not the
    /// other is worth a rule rather than a review note.
    #[test]
    fn a_moved_handler_that_writes_a_value_guards_on_pressed() {
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            for (at, _) in source.match_indices("moved =>") {
                let line_start = source[..at].rfind('\n').map_or(0, |index| index + 1);
                if source[line_start..at].contains("//") {
                    continue;
                }
                let body = handler_body(&source, at);
                if !assigns_a_property(&body) {
                    continue;
                }
                checked += 1;
                if body.contains(".pressed") {
                    continue;
                }
                let line = source[..at].matches('\n').count() + 1;
                offenders.push(format!("{name}:{line}: a moved handler assigns without a pressed guard"));
            }
        }
        assert!(
            checked >= 2,
            "expected the viewer's pan handler and the slider's drag handler, found {checked}"
        );
        assert!(
            offenders.is_empty(),
            "a pointer move alone is a hover: a handler that writes there has to ask \
             after-pressed first:\n{}",
            offenders.join("\n")
        );
    }

    /// G45 — a hover surface declared after a pointer target eats that target's press.
    ///
    /// Slint hit-tests in reverse declaration order, and a `TouchArea` claims the mouse on
    /// `MouseEvent::Pressed` whether or not it carries a handler. A `TouchArea` that owns
    /// no `clicked` is nothing but a hover/hit surface, and declared *after* the controls
    /// it shares a row with it is above them: the control still draws its hover, still
    /// answers the keyboard, and never receives the press. That is the shape
    /// `downloads.slint`'s local-record row shipped — open, reveal, re-download and forget
    /// were pointer-dead while Tab still reached them — and the reason the surface has to
    /// be declared first.
    ///
    /// A `TouchArea` that carries its own `clicked` *is* the target and belongs last, and
    /// a `FocusScope` only takes the pointer after a press has been claimed, so neither is
    /// what this rule is about. Both may sit under the controls.
    #[test]
    fn a_hover_surface_declared_after_a_pointer_target_loses_the_press() {
        let targets = pointer_targets();
        assert!(
            targets.len() >= 4,
            "expected the shared controls to declare their pointer callbacks, found {}: {targets:?}",
            targets.len()
        );
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            let blocks = element_blocks(&source);
            for block in blocks.iter().filter(|block| block.name == "TouchArea") {
                let own = own_lines(&block.body);
                if owns_click(&own) {
                    continue;
                }
                // Only a surface that spans its parent can be over anything: one that
                // states its own box is a small target of its own, and where it sits
                // relative to a sibling control is then a layout question, not this one.
                if ["width:", "height:", "x:", "y:"]
                    .iter()
                    .any(|key| own.lines().any(|line| line.trim_start().starts_with(key)))
                {
                    continue;
                }
                checked += 1;
                let owner = enclosing_component(&source, source[..block.start].matches('\n').count());
                let here = source[..block.start].matches('\n').count() + 1;
                for earlier in blocks.iter().filter(|other| other.start < block.start) {
                    if !targets.contains(&earlier.name) {
                        continue;
                    }
                    if enclosing_component(&source, source[..earlier.start].matches('\n').count())
                        != owner
                    {
                        continue;
                    }
                    let there = source[..earlier.start].matches('\n').count() + 1;
                    offenders.push(format!(
                        "{name}:{here}: a hover-only TouchArea covers {} at line {there}, \
                         declared before it",
                        earlier.name
                    ));
                }
            }
        }
        assert!(
            checked >= 2,
            "expected the row wash and the viewer's pan surface to be found, found {checked}; \
             the scan is broken"
        );
        assert!(
            offenders.is_empty(),
            "a TouchArea without a click of its own goes *before* the controls it shares a \
             row with: declared after them it takes their press and they are pointer-dead:\n{}",
            offenders.join("\n")
        );
    }
    /// The `key:` binding of a block, as written: from the key to its terminating `;`.
    ///
    /// Taken as text rather than as a number because the question is *which tokens* an
    /// anchor names — a geometry rule that only read the value would pass on a literal
    /// that happens to equal the token today.
    fn geometry_binding(body: &str, key: &str) -> Option<String> {
        let prefix = format!("{key}:");
        let mut offset = 0usize;
        for line in body.split_inclusive('\n') {
            let code = line.split("//").next().unwrap_or("");
            if code.trim_start().starts_with(&prefix) {
                let start = offset + (code.len() - code.trim_start().len());
                let rest = &body[start..];
                let end = rest.find(';').map_or(rest.len(), |at| at + 1);
                return Some(rest[..end].to_string());
            }
            offset += line.len();
        }
        None
    }

    /// G47 — the notice bar's and the batch bar's `y` ranges must not intersect.
    ///
    /// The batch bar is anchored to the bottom of the page box on three destinations, and
    /// the shell's notice bar is anchored to the bottom of the content area, which is the
    /// same box: `H - 44 - 16` for the notice and `H - 48 - 12` for the bar, so the notice
    /// covered all 44 px of the bar. The notice takes no input except its own close
    /// button, so the clicks aimed at the buttons it was covering went *through* it to the
    /// bar underneath — a batch download that raised a notice hid the bar it came from and
    /// left it clickable. The anchor has to subtract the bar it is above, and it has to
    /// ask `selection-mode`: the bar only exists then.
    #[test]
    fn the_notice_bar_clears_the_batch_bar_in_selection_mode() {
        let source = read(&ui_dir().join("main.slint"));
        let blocks = element_blocks(&source);
        let notice = blocks
            .iter()
            .find(|block| opener(&source, block).contains("root.notice != \"\""))
            .expect("the shell must draw a notice bar");
        let anchor = geometry_binding(&notice.body, "y")
            .expect("the notice bar must state its own y");
        assert!(
            anchor.contains("batch-bar-height"),
            "the notice bar is anchored at the bottom of the content area, where the batch \
             bar floats. Subtract Theme.batch-bar-height from its y:\n{anchor}"
        );
        assert!(
            anchor.contains("selection-mode"),
            "the subtraction has to ask selection-mode: the batch bar exists only then:\n{anchor}"
        );
    }

    /// G48 — two overlays in one component may not share an `(x, y)` anchor.
    ///
    /// An overlay in a card or a page states its own corner: the rank badge the top-left,
    /// the type badge the top-right, the bookmark pill the bottom-right, the selection
    /// check wherever it was last moved. Two of them on the same pair therefore means one
    /// is drawn on top of the other, and because the later declaration wins the loser is
    /// not misplaced — it is invisible. That is how selection mode hid `#1…#N` on the
    /// ranking page: the check and the rank badge were both at `(space-2, space-2)`.
    ///
    /// Only a *conditional* box that states all four of its sides counts as an overlay:
    /// a centred glyph, an icon slot and a layout cell have geometry too, and none of
    /// them competes for a corner.
    #[test]
    fn two_overlays_in_one_component_do_not_share_an_anchor() {
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            let blocks = element_blocks(&source);
            let mut seen: std::collections::BTreeMap<(String, String, String), Vec<usize>> =
                std::collections::BTreeMap::new();
            for block in &blocks {
                if !opener(&source, block).contains("if ") {
                    continue;
                }
                let own = own_lines(&block.body);
                let has = |key: &str| {
                    own.lines()
                        .any(|line| line.trim_start().starts_with(&format!("{key}:")))
                };
                if !(has("x") && has("y") && has("width") && has("height")) {
                    continue;
                }
                let (Some(x), Some(y)) = (
                    geometry_binding(&block.body, "x"),
                    geometry_binding(&block.body, "y"),
                ) else {
                    continue;
                };
                // Two full-surface parts at the origin are not competing for a
                // corner: the palette's click-catcher and the suggestion panel's are
                // whole-window boxes, they are never on screen together, and the pair
                // says nothing about where anything inside them sits.
                if x.trim() == "x: 0px;" && y.trim() == "y: 0px;" {
                    continue;
                }
                checked += 1;
                let owner =
                    enclosing_component(&source, source[..block.start].matches('\n').count());
                let line = source[..block.start].matches('\n').count() + 1;
                seen.entry((owner, x.trim().to_string(), y.trim().to_string()))
                    .or_default()
                    .push(line);
            }
            for ((owner, x, y), lines) in seen {
                if lines.len() < 2 {
                    continue;
                }
                offenders.push(format!(
                    "{name}:{owner}: lines {lines:?} share the anchor {x} / {y}"
                ));
            }
        }
        assert!(
            checked >= 8,
            "expected the cards and the shell to anchor several overlays, found {checked}"
        );
        assert!(
            offenders.is_empty(),
            "two overlays on one anchor: the later declaration hides the earlier one. Give \
             each its own corner:\n{}",
            offenders.join("\n")
        );
    }

    /// G49 — a component that declares a default accessible action owns a `FocusScope`.
    ///
    /// `accessible-action-default` is the action a screen reader invokes when the user
    /// activates the element, so the element has to be reachable as one node by the
    /// keyboard too. Round eleven wired the roles and left the question "and can the
    /// keyboard get here" to review, which is how a `TagChip` whose `FocusScope` sat inside
    /// its glyph, and a `MenuRow` that answered an action it could never receive, both
    /// looked finished. Each exemption below is a *reason*, not a silence: a listbox row is
    /// reachable through the list's own scope, and the pill is reachable through the card's
    /// keys.
    #[test]
    fn every_default_action_owns_a_focus_scope_or_a_reason() {
        const EXEMPT: [(&str, &str); 5] = [
            (
                "TextField",
                "the field is the Tab stop; its default action focuses its own input",
            ),
            (
                "MenuRow",
                "a menu is a listbox: BookmarkMenu owns the scope and moves the selection",
            ),
            (
                "PaletteRowView",
                "the palette owns the scope; the row is moved by the arrow keys",
            ),
            (
                "SuggestRow",
                "the suggestion panel owns the scope, for the same reason",
            ),
            (
                "BookmarkPill",
                "IllustCard's 'b' is the pill's keyboard path (the card is the stop)",
            ),
        ];
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in page_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let source = read(&path);
            let blocks = element_blocks(&source);
            let mut offset = 0usize;
            for (index, raw) in source.split_inclusive('\n').enumerate() {
                let code = raw.split("//").next().unwrap_or("");
                let Some(found) = code.find("accessible-action-default") else {
                    offset += raw.len();
                    continue;
                };
                // A dismissal backdrop is exempt, by the convention the hit-surface rule
                // already uses: the surface that exists to be clicked away names itself
                // `*-scrim`, Escape is its keyboard path, and the action it declares
                // duplicates the control that opened the panel.
                let at = offset + found;
                let backdrop = blocks
                    .iter()
                    .filter(|block| block.start < at && block.end > at)
                    .min_by_key(|block| block.end - block.start)
                    .map(|block| opener(&source, block))
                    .unwrap_or_default()
                    .contains("scrim :=");
                // The owner is the nearest component declaration above the action: a body
                // that talks about an action somewhere else is not an answer.
                let owner = enclosing_component(&source, index);
                offset += raw.len();
                checked += 1;
                if backdrop {
                    continue;
                }
                let declares_scope = source
                    .lines()
                    .position(|line| {
                        let code = line.split("//").next().unwrap_or("");
                        code.contains(&format!("component {owner} "))
                    })
                    .map(|at| component_body(&source, at).contains("FocusScope"))
                    .unwrap_or(false);
                if declares_scope || EXEMPT.iter().any(|(exempt, _reason)| *exempt == owner) {
                    continue;
                }
                offenders.push(format!(
                    "{name}:{}: {owner} answers a default action and cannot be focused",
                    index + 1
                ));
            }
        }
        assert!(
            checked >= 12,
            "expected every shared control to declare its default action, found {checked}"
        );
        assert!(
            offenders.is_empty(),
            "a default action on an element the keyboard cannot reach is a screen-reader \
             verb that does nothing. Give it a FocusScope, or add it to EXEMPT with the \
             scope that serves it instead:\n{}",
            offenders.join("\n")
        );
    }

    /// G50 — every `out property <length>` of `Theme` is read from the UI.
    ///
    /// The motion-token rule (`every_declared_motion_token_is_referenced_…`) asks this of
    /// `<duration>` because a token nothing reads is a rule that quietly stopped applying.
    /// A *length* is the same fact one lane over, and this round found the sharpest version
    /// of it: `reader-font-min`/`max` and `reader-measure-min`/`max` were declared, mirrored
    /// against `prefs.rs` by a test, and read by nobody — the reader's sliders computed
    /// `14 + ratio * 18` and `420 + ratio * 980` from their own literals, so the bounds had
    /// two sources, one of them the theme, and the one a user saw was the other. Each token
    /// is now interpolated from the theme; putting a literal back reds this rule.
    ///
    /// `color accent` is excluded by construction: `accent_is_decoration_only` forbids a
    /// reader on purpose, so it can never pass this rule and must not be asked.
    #[test]
    fn every_theme_length_token_is_read_by_the_ui() {
        let theme = read(&ui_dir().join("theme.slint"));
        // Only the `Theme` global: `Type` below it is an int ramp read as `Type.semibold`.
        let theme = theme.split("export global Type").next().unwrap_or("").to_string();
        let readers: Vec<String> = page_files()
            .into_iter()
            .filter(|path| path.file_name().and_then(|n| n.to_str()) != Some("theme.slint"))
            .map(|path| read(&path))
            .collect();
        let mut offenders = Vec::new();
        let mut checked = 0;
        for (index, line) in theme.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let Some(rest) = code.trim_start().strip_prefix("out property <length> ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if name.is_empty() {
                continue;
            }
            checked += 1;
            if readers.iter().any(|source| reads_token(source, &name)) {
                continue;
            }
            offenders.push(format!("theme.slint:{}: {name} has no reader in ui/", index + 1));
        }
        assert!(
            checked >= 30,
            "expected the theme's length table, found {checked} tokens"
        );
        assert!(
            offenders.is_empty(),
            "a length token no page reads is a design decision that stopped applying. Read \
             it, or delete it:\n{}",
            offenders.join("\n")
        );
    }

    /// G51 — the indeterminate progress bar animates, and from `Theme.motion`.
    ///
    /// A bar that says "still working" and does not move is the one thing the widget may
    /// not do: every running download showed the same static 30 % block, so a queue of six
    /// looked like six stalls. The fix is an `animate` whose duration is the theme's motion
    /// token and whose loop is the animation engine's (`iteration-count: -1` with
    /// `direction: alternate`), not a timer element: no tick, no extra property, and the
    /// 150 ms ceiling the style contract sets is the one already in `Theme.motion`.
    #[test]
    fn an_indeterminate_progress_bar_loops_from_the_motion_token() {
        let source = read(&ui_dir().join("components.slint"));
        let at = source
            .lines()
            .position(|line| line.contains("component ProgressBar "))
            .expect("the shared progress bar must exist");
        let body = component_body(&source, at);
        assert!(
            body.contains("indeterminate"),
            "ProgressBar is the component this rule is about; it must still declare the flag"
        );
        assert!(
            body.lines().any(|line| line.trim_start().starts_with("animate x")),
            "the indeterminate thumb has to move: without an `animate x` the bar is a still \
             picture of progress"
        );
        assert!(
            body.contains("duration: Theme.motion"),
            "the motion has to be the theme's: no animate in this tree carries a literal \
             duration (see the motion-token rule)"
        );
        assert!(
            body.contains("iteration-count"),
            "and it has to loop: one transition would leave the thumb parked at the end of \
             the track, which is a worse lie than standing still"
        );
    }

}
