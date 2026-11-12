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
