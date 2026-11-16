use super::*;

/// `PIXIV_DESKTOP_FIXTURE`: a deterministic state a capture can ask for (G24).
///
/// The variable was named in three plans and implemented nowhere, which is why
/// the states round nine creates had no way to be photographed at all. This file
/// is the app half of `tools/fixture.ps1`: it reads the same value and puts the
/// shell in the state the table describes. Nothing here runs unless the variable
/// names one of them - an unset variable is not a fixture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fixture {
    None,
    Selection,
    Palette,
    Suggestion,
    /// F1, owned by the detail track.
    Ugoira,
    /// F5, owned by the downloads track.
    Downloads,
    /// F2, this track: the opened work is marked sensitive, which the demo catalogue
    /// cannot show — every one of its rows carries `is_masked: false`.
    Sensitive,
    /// F4, this track: every third work in a demo page is AI-generated, for the card
    /// badge. The catalogue's rows all carry `aiType: 0`.
    Ai,
    /// A value this build does not know. `tools/fixture.ps1` exits 77 for it, so
    /// the app must not pretend it is the same as no fixture.
    Unknown,
}

/// The value, parsed. Pure, so the table can be tested without a window.
pub(crate) fn fixture_from(value: Option<&str>) -> Fixture {
    let Some(value) = value.map(str::trim) else {
        return Fixture::None;
    };
    match value.to_ascii_lowercase().as_str() {
        "" => Fixture::None,
        "selection" => Fixture::Selection,
        "palette" => Fixture::Palette,
        "suggestion" => Fixture::Suggestion,
        "ugoira" => Fixture::Ugoira,
        "downloads" => Fixture::Downloads,
        "sensitive" => Fixture::Sensitive,
        "ai" => Fixture::Ai,
        _ => Fixture::Unknown,
    }
}

/// The ambient value, read once.
pub(crate) fn current() -> Fixture {
    static CACHE: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| fixture_from(std::env::var("PIXIV_DESKTOP_FIXTURE").ok().as_deref()))
}

thread_local! {
    /// Whether the fixture has been put in place. A fixture is *start-up* state,
    /// so it is applied once - except for the selection one, which waits for the
    /// first feed to have rows to select.
    static APPLIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl App {
    /// Put the shell in the requested fixture, once.
    ///
    /// Called at the end of a feed render rather than at install: `App::navigate` closes
    /// the suggestion panel on its way to the start page, so a panel filled
    /// before that was cleared again long before the first frame.
    pub(crate) fn apply_fixtures(&mut self) {
        if APPLIED.with(|flag| flag.get()) {
            return;
        }
        let applied = match current() {
            Fixture::Selection => self.selection_fixture(3),
            Fixture::Palette => {
                self.palette_fixture();
                true
            }
            Fixture::Suggestion => {
                self.suggest_fixture();
                true
            }
            // Not this track's state, or not a state at all: nothing to do, and no
            // reason to re-check on every render.
            // A grid fixture is applied by the render that builds the rows
            // (`demo_items`), not here.
            Fixture::Sensitive
            | Fixture::Ai
            | Fixture::Ugoira
            | Fixture::Downloads
            | Fixture::Unknown
            | Fixture::None => true,
        };
        if applied {
            APPLIED.with(|flag| flag.set(true));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset is not unknown: every run before this round had no variable and must
    /// keep behaving exactly as it did.
    #[test]
    fn only_a_named_value_is_a_fixture() {
        assert_eq!(fixture_from(None), Fixture::None);
        assert_eq!(fixture_from(Some("")), Fixture::None);
        assert_eq!(fixture_from(Some("   ")), Fixture::None);
        assert_eq!(fixture_from(Some("selection")), Fixture::Selection);
        assert_eq!(fixture_from(Some(" Palette ")), Fixture::Palette);
        assert_eq!(fixture_from(Some("suggestion")), Fixture::Suggestion);
        assert_eq!(fixture_from(Some("ugoira")), Fixture::Ugoira);
        assert_eq!(fixture_from(Some("downloads")), Fixture::Downloads);
        assert_eq!(fixture_from(Some("sensitive")), Fixture::Sensitive);
        assert_eq!(fixture_from(Some("AI")), Fixture::Ai);
        // Not a fixture this build knows: 77 in tools/fixture.ps1, never "no fixture".
        assert_eq!(fixture_from(Some("still-nonsense")), Fixture::Unknown);
    }
}
