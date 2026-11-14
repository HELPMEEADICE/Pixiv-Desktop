use super::*;

/// Multi-select on a grid page (round nine, F2).
///
/// The model is `Selection` (src/app/selection.rs); this file is the half that
/// talks to Slint: which property carries the mode, which rows get the check, and
/// what a click with or without a modifier means.
impl App {
    /// The works the page is showing, in load order - the order a Shift range and
    /// the published count are both defined over.
    pub(crate) fn selection_order(&self) -> Vec<u64> {
        self.items.iter().map(|illust| illust.id.get()).collect()
    }

    /// Enter or leave the mode, and repaint every card.
    pub(crate) fn selection_set_mode(&mut self, active: bool) {
        self.selection.set_mode(active);
        self.publish_selection();
    }

    /// A feed reload: the rows the ids named are gone.
    pub(crate) fn selection_reset(&mut self) {
        if !self.selection.active && self.selection.is_empty() && self.selection.anchor.is_none() {
            return;
        }
        self.selection.reset();
        self.publish_selection();
    }

    /// A card that is built while a selection is active carries the mode's flags.
    ///
    /// A column rebuild (a resize that changes the column count, a reload) creates
    /// every row from scratch, and a row built without these two flags draws no
    /// check and opens on a plain click - while the count in the batch bar still
    /// says three. That drift is why the builder calls this, not only the toggle
    /// path.
    pub(crate) fn mark_selected(&self, item: &mut IllustItem) {
        let Ok(id) = item.id.as_str().parse::<u64>() else {
            return;
        };
        item.selectable = self.selection.active;
        item.selected = self.selection.contains(id);
    }

    /// Two properties and the cards' own flags, from one call.
    pub(crate) fn publish_selection(&self) {
        let ui = self.ui();
        ui.set_selection_mode(self.selection.active);
        ui.set_selected_count(self.selection.ids.len() as i32);
        self.sync_selected_rows();
    }

    /// Write `selected` and `selectable` back into the column models.
    ///
    /// `set_row_data` rather than a rebuilt model: the grid is a set of
    /// `Rc<VecModel<IllustItem>>` that a reload is allowed to replace and a
    /// selection is not (the same rule `resync_bookmarks` follows). `selectable` is
    /// what draws the 22 px check at all, so it is set from the mode even on the
    /// rows that are not chosen.
    fn sync_selected_rows(&self) {
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                let Ok(id) = item.id.as_str().parse::<u64>() else {
                    continue;
                };
                let want_selected = self.selection.contains(id);
                if item.selected != want_selected || item.selectable != self.selection.active {
                    item.selected = want_selected;
                    item.selectable = self.selection.active;
                    column.set_row_data(row, item);
                }
            }
        }
    }
}

impl App {
    /// One card was clicked, with whatever modifiers arrived.
    ///
    /// The rule, in the order the plan states it: a plain click opens unless the
    /// page is in selection mode, in which case it toggles; Ctrl always toggles
    /// and enters the mode; Shift selects the range from the anchor in
    /// `self.items` order.
    pub(crate) fn card_clicked(shared: &Shared, id: &str, ctrl: bool, shift: bool) {
        let Ok(raw) = id.trim().parse::<u64>() else {
            return;
        };
        let open = {
            let mut app = shared.borrow_mut();
            // The page owns `selection-mode` (its header toggle writes the same
            // property), so the intent is read before it is acted on.
            app.reconcile_shell_state();
            let order = app.selection_order();
            if shift {
                if !app.selection.active {
                    app.selection.active = true;
                }
                if app.selection.anchor.is_none() {
                    app.selection.toggle(raw, &order);
                }
                app.selection.extend_to(raw, &order);
                if app.selection.anchor.is_none() {
                    app.selection.anchor_to(raw, &order);
                }
                app.publish_selection();
                false
            } else if ctrl || app.selection.active {
                if ctrl {
                    app.selection.active = true;
                }
                app.selection.toggle(raw, &order);
                app.selection.anchor_to(raw, &order);
                app.publish_selection();
                false
            } else {
                true
            }
        };
        if open {
            App::open_detail(shared, id.trim());
        }
    }

    /// Ctrl+A, and the batch bar's 「全选」: everything this page has loaded.
    pub(crate) fn select_all_loaded(shared: &Shared) {
        let mut app = shared.borrow_mut();
        app.reconcile_shell_state();
        let order = app.selection_order();
        app.selection.active = true;
        app.selection.select_all(&order);
        app.publish_selection();
    }

    /// The batch bar's 「取消选择」: the mode stays, the ids go.
    pub(crate) fn clear_selection(shared: &Shared) {
        let mut app = shared.borrow_mut();
        app.reconcile_shell_state();
        app.selection.ids.clear();
        app.selection.anchor = None;
        app.publish_selection();
    }

    /// Escape, 「完成」 and the page's own toggle: leave the mode entirely.
    pub(crate) fn exit_selection(shared: &Shared) {
        let mut app = shared.borrow_mut();
        app.selection_set_mode(false);
    }
}
impl App {
    /// PIXIV_DESKTOP_FIXTURE=selection: the home grid, with its first three works
    /// chosen and the batch bar therefore drawn.
    ///
    /// Returns whether there was a feed to select from; the dispatcher only marks
    /// the fixture applied when this is true, so an empty grid does not swallow the
    /// state a capture was asked for.
    pub(crate) fn selection_fixture(&mut self, count: usize) -> bool {
        let order = self.selection_order();
        if order.is_empty() {
            return false;
        }
        self.selection.active = true;
        self.selection.ids = order[..order.len().min(count)].to_vec();
        self.selection.anchor_to(order[0], &order);
        self.publish_selection();
        true
    }
}
