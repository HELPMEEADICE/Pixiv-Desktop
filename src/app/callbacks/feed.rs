use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
    // F1: the vocabulary the two segmented controls draw is this crate's, and it
    // exists before any request does — a control that appears only after the first
    // reply is a control the user cannot use to make it.
    app.borrow().publish_controls();
    {
        let shared = app.clone();
        ui.on_toggle_bookmark(move |id| {
            let id = id.to_string();
            let mut app = shared.borrow_mut();
            app.toggle_bookmark(&id);
        });
    }

    {
        let shared = app.clone();
        ui.on_open_tag(move |tag| {
            let tag = tag.to_string();
            {
                let mut app = shared.borrow_mut();
                app.search_word = tag.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&tag);
                // A history change: the chips are published from the cache now, so
                // the store write has to refresh it here (plan §2.3 R16).
                app.reload_recent_searches();
            }
            App::navigate(&shared, Destination::Search);
        });
    }

    {
        let shared = app.clone();
        ui.on_grid_measured(move |width| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.on_measured(width);
        });
    }

    {
        let shared = app.clone();
        ui.on_grid_scrolled(move |offset| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            let route = app.route;
            app.remember_scroll(route, offset);
        });
    }

    {
        let shared = app.clone();
        ui.on_grid_retry(move || {
            let page = shared.borrow().page;
            match page {
                Destination::Home => App::load_home(&shared),
                Destination::Discovery => App::load_discovery(&shared, false),
                Destination::UserProfile => App::load_profile(&shared),
                Destination::Detail => {
                    let id = shared.borrow().detail.as_ref().map(|d| d.id.get());
                    if let Some(id) = id {
                        App::open_detail_route(&shared, id, false);
                    }
                }
                // These two have no feed to reload. Ctrl+R means "refresh what is
                // in front of me", and on these pages that is the queue and the
                // cache accounting - not a silently re-fetched grid nobody sees.
                Destination::Downloads => {
                    let app = shared.borrow();
                    app.push_downloads();
                    app.refresh_chrome();
                }
                Destination::Settings => {
                    let app = shared.borrow();
                    app.refresh_chrome();
                }
                _ => App::load_page(&shared, false),
            }
        });
    }

    {
        let shared = app.clone();
        ui.on_grid_action(move || App::navigate(&shared, Destination::Settings));
    }

    {
        let shared = app.clone();
        ui.on_load_more(move || App::load_more(&shared));
    }

    // ---- round twelve (F1): the search surface's controls ----
    //
    // The page reports the index a gesture picked and never writes its own
    // `sort-index`/`mode-index`; these three write the model and then ask for page 1
    // again through `reload_feed`, which is the one place that resets the cursor and
    // bumps the epoch — so a reply for the old parameters cannot land.
    {
        let shared = app.clone();
        ui.on_search_sort_chosen(move |index| {
            App::control_chosen(&shared, |controls| controls.with_sort_index(index));
        });
    }

    {
        let shared = app.clone();
        ui.on_search_mode_chosen(move |index| {
            App::control_chosen(&shared, |controls| controls.with_target_index(index));
        });
    }

    {
        let shared = app.clone();
        ui.on_search_related_tag_chosen(move |tag| {
            // One implementation of "run this word", shared with the search box, the
            // suggestion panel and the palette.
            App::submit_search(&shared, tag.as_str());
        });
    }

    {
        let shared = app.clone();
        ui.on_select_ranking_filter(move |index| {
            {
                let mut app = shared.borrow_mut();
                let kinds = RankingKind::ALL;
                let Some(kind) = usize::try_from(index)
                    .ok()
                    .and_then(|i| kinds.get(i).copied())
                else {
                    return;
                };
                if kind == app.ranking_kind && app.page == Destination::Ranking {
                    return;
                }
                app.ranking_kind = kind;
                app.feed_owner = None;
                app.ui().set_ranking_filter(index);
            }
            if shared.borrow().page != Destination::Ranking {
                App::navigate(&shared, Destination::Ranking);
            } else {
                App::reload_feed(&shared);
            }
        });
    }

    // ---- round nine: multi-select (F2) ----
    //
    // One set of verbs for the three grid pages: the page reports the gesture and
    // the shell owns the state, which is what keeps `Selection` the only
    // copy of it (ui/main.slint binds the pages to the same two properties).
    {
        let shared = app.clone();
        ui.on_card_clicked(move |id, ctrl, shift| {
            let id = id.to_string();
            App::card_clicked(&shared, &id, ctrl, shift);
        });
    }

    // ---- round eleven (U-f): focus scrolls the page ----
    //
    // `IllustCard.gained-focus` fired and nothing followed it: the card reported that
    // the Tab walk had arrived, and the page stayed where it was, so a keyboard user
    // was told about a card forty rows below the viewport. The card now reports its own
    // `y` in the scroll body (`ArtworkGrid` adds the grid's offset), and this is the
    // only place that knows the viewport, so this is where "focused" becomes "on
    // screen". Offsets are negative-down: `PageScroll.scrolled` reports
    // `0px - content-y`, so a card at `y` lands at `-(y - lead)`.
    {
        let shared = app.clone();
        ui.on_card_focused(move |id, y| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            // A card that a route change removed is not chased. The grid that reported
            // the focus may already belong to the page behind this one, and scrolling a
            // page the user is not looking at is the same class of defect the gate is
            // about (plan §2.1 U-f).
            let Some(route) = app.feed_owner else {
                return;
            };
            if route != app.route {
                return;
            }
            let Ok(work_id) = id.as_str().parse::<u64>() else {
                return;
            };
            if !app
                .items
                .iter()
                .any(|work| work.id == pixiv_types::Id(work_id))
            {
                return;
            }
            let offset = card_scroll_offset(y, FOCUS_LEAD);
            app.remember_scroll(route, offset);
            let ui = app.ui();
            if app.page == Destination::Home {
                ui.set_home_scroll_y(offset);
            } else {
                ui.set_grid_scroll_y(offset);
            }
        });
    }

    {
        let shared = app.clone();
        ui.on_select_all(move || App::select_all_loaded(&shared));
    }

    {
        let shared = app.clone();
        ui.on_clear_selection(move || App::clear_selection(&shared));
    }

    {
        let shared = app.clone();
        ui.on_batch_download(move || App::batch_download(&shared));
    }

    {
        let shared = app.clone();
        ui.on_batch_bookmark(move || App::batch_bookmark(&shared));
    }

    // ---- round nine: the command palette (F3) ----
    //
    // `Ctrl+P` flips `palette-open` inside the UI; the rows, the cursor and
    // everything a row *does* are here. The reconcile timer is what turns the
    // flip into an opened panel (see feed/reconcile.rs).
    {
        let shared = app.clone();
        ui.on_palette_query(move |text| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.palette_query(text.as_str());
        });
    }

    {
        let shared = app.clone();
        ui.on_palette_choose(move |index| App::palette_choose(&shared, index));
    }

    {
        let shared = app.clone();
        ui.on_palette_close(move || {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.palette_close();
        });
    }

    start_reconcile(app);
}

/// How far a page must scroll to put a card that took the focus on screen.
///
/// `y` is the card's offset in the scroll body's own coordinates and every page offset
/// in this app is negative-down (`PageScroll.scrolled` reports `0px - content-y`), so the
/// target is `-(y - lead)`: one lead step above the card, which keeps the row it belongs
/// to visible instead of pinning its top edge to the page's first pixel.
///
/// Clamped to `<= 0` in both directions and defined for the first screen: a card inside
/// the first `lead` pixels scrolls nowhere at all (a positive offset is not a scroll
/// position), and a non-finite measurement is treated as "do not move" rather than
/// handed to the ScrollView.
fn card_scroll_offset(y: f32, lead: f32) -> f32 {
    if !y.is_finite() {
        return 0.0;
    }
    (0.0 - (y - lead).max(0.0)).min(0.0)
}

/// The one lead step: one `Theme.space-3`, 12 logical px.
const FOCUS_LEAD: f32 = 12.0;

#[cfg(test)]
mod tests {
    use super::card_scroll_offset;

    #[test]
    fn a_focused_card_scrolls_in_with_one_lead_step_and_never_past_the_top() {
        assert_eq!(
            card_scroll_offset(0.0, 12.0),
            0.0,
            "the first card moves nothing"
        );
        assert_eq!(
            card_scroll_offset(6.0, 12.0),
            0.0,
            "inside the lead, still nothing"
        );
        assert_eq!(
            card_scroll_offset(212.0, 12.0),
            -200.0,
            "one lead above the card"
        );
        assert_eq!(
            card_scroll_offset(f32::NAN, 12.0),
            0.0,
            "a measurement that is not a number must not be handed to the view"
        );
    }
}
