use super::*;

/// How often the two page-owned flags are read back.
///
/// Slint generates no changed-handler for a property that declares no callback, and
/// `selection-mode` and `palette-open` are both written from inside the UI
/// (the page's 「多选」 toggle, `Ctrl+P`). Reading them is therefore the only way Rust
/// can learn about them; 50 ms keeps the window between the click and the
/// consequence shorter than a double click, and costs one property read per tick.
const RECONCILE_MS: u64 = 50;

thread_local! {
    /// One timer for the life of the window. It lives here because it owns nothing
    /// any other module reads, and a `Timer` is a handle onto a native timer only
    /// the UI thread may own.
    static RECONCILE: slint::Timer = slint::Timer::default();
}

pub(crate) fn start_reconcile(app: &Shared) {
    let shared = app.clone();
    RECONCILE.with(|timer| {
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(RECONCILE_MS),
            move || {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.reconcile_shell_state();
            },
        );
    });
}

impl App {
    /// Adopt whatever the page or the keyboard has already decided.
    ///
    /// Two properties are `in-out` and written from Slint: the palette opens on
    /// `Ctrl+P` and selection mode is entered by the page's own toggle (the
    /// fallback the plan names for modifiers that may never arrive). Both are
    /// published back here so Rust stays the single owner of the rows and the ids
    /// without fighting the UI for the flag itself.
    pub(crate) fn reconcile_shell_state(&mut self) {
        let ui = self.ui();
        let palette_open = ui.get_palette_open();
        if palette_open != self.palette.open {
            if palette_open {
                self.palette_open();
            } else {
                self.palette_close();
            }
        }
        // The caret is the field's; this keeps Rust's copy where the panel draws
        // it, and clamps it back if the rows shrank under it.
        let caret = ui.get_palette_active();
        if caret != self.palette.active {
            let delta = caret - self.palette.active;
            self.palette_move(delta);
        }
        let selection = ui.get_selection_mode();
        if selection != self.selection.active {
            self.selection_set_mode(selection);
        }
    }
}
