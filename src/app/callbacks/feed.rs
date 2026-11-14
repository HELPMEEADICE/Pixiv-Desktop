use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
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
