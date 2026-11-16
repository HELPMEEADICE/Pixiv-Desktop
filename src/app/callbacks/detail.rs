use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
    {
        let shared = app.clone();
        ui.on_open_illust(move |id| App::open_detail(&shared, id.as_str()));
    }

    // ---- detail ----
    {
        let shared = app.clone();
        ui.on_detail_back(move || App::travel(&shared, true));
    }

    // F2: 「显示」 on a work Pixiv marks sensitive. The mask is the app's state, not
    // the page's: lifting it is what lets the viewer request the full-size page.
    {
        let shared = app.clone();
        ui.on_detail_reveal(move || {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.reveal_masked();
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_toggle_bookmark(move || {
            let id = {
                let app = shared.borrow();
                app.detail.as_ref().map(|d| d.id.to_string())
            };
            if let Some(id) = id {
                let mut app = shared.borrow_mut();
                app.toggle_bookmark(&id);
            }
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_open_author(move || {
            let id = {
                let app = shared.borrow();
                app.detail.as_ref().map(|d| d.author.id.to_string())
            };
            if let Some(id) = id {
                App::open_profile(&shared, &id);
            }
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_open_tag(move |tag| {
            let tag = tag.to_string();
            {
                let mut app = shared.borrow_mut();
                app.search_word = tag.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&tag);
                // Same rule as the tag chip on a card: the store write is a history
                // change, so the cached words are refreshed here (plan §2.3 R16).
                app.reload_recent_searches();
            }
            App::navigate(&shared, Destination::Search);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_download_page(move || {
            let mut app = shared.borrow_mut();
            app.enqueue_download(false);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_download_all(move || {
            let mut app = shared.borrow_mut();
            app.enqueue_download(true);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_export_ugoira(move || {
            let mut app = shared.borrow_mut();
            app.export_ugoira();
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_follow(move || {
            let mut app = shared.borrow_mut();
            app.toggle_follow();
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_open_external(move || {
            let app = shared.borrow();
            if let Some(detail) = &app.detail {
                let _ = open::that(format!("https://www.pixiv.net/artworks/{}", detail.id));
            }
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_retry(move || {
            let id = {
                let app = shared.borrow();
                app.detail.as_ref().map(|d| d.id.to_string())
            };
            if let Some(id) = id {
                App::open_detail(&shared, &id);
            }
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_select_page(move |index| {
            let mut app = shared.borrow_mut();
            // Selecting a page also plans the decodes around it: the viewer only
            // ever holds the current page and its neighbours (O1).
            app.show_page(index);
        });
    }

    // ---- round nine: F1 playback, F4 visibility, U29 stepping ----
    {
        let shared = app.clone();
        ui.on_toggle_playback(move || {
            let mut app = shared.borrow_mut();
            app.toggle_playback();
        });
    }

    {
        let shared = app.clone();
        ui.on_page_step(move |delta| {
            let mut app = shared.borrow_mut();
            app.page_step(delta);
        });
    }

    {
        let shared = app.clone();
        ui.on_pick_restrict(move |restrict| {
            let mut app = shared.borrow_mut();
            app.pick_restrict(restrict);
        });
    }

    // ---- viewer transform: all of it in `zoom`, all of it tested ----
    {
        let shared = app.clone();
        ui.on_detail_request_zoom(move |cursor_x, cursor_y, delta_y, view_w, view_h| {
            let app = shared.borrow();
            let ui = app.ui();
            let viewport = crate::zoom::Viewport::new(view_w, view_h);
            let current = ui.get_detail_zoom().max(crate::zoom::ZOOM_MIN);
            let next = crate::zoom::wheel_zoom(current, delta_y);
            if (next - current).abs() < f32::EPSILON {
                return;
            }
            let pan = crate::zoom::Pan {
                x: ui.get_detail_pan_x(),
                y: ui.get_detail_pan_y(),
            };
            let moved =
                crate::zoom::anchored_pan(pan, (cursor_x, cursor_y), viewport, next / current);
            let (image_w, image_h) = current_image_size(&ui, &app);
            let scale = crate::zoom::effective_scale(
                viewport,
                (image_w, image_h),
                ui.get_detail_fit_mode(),
                next,
            );
            let clamped = crate::zoom::clamp_pan(moved, viewport, (image_w, image_h), scale);
            ui.set_detail_zoom(next);
            ui.set_detail_pan_x(clamped.x);
            ui.set_detail_pan_y(clamped.y);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_request_pan(move |delta_x, delta_y| {
            let app = shared.borrow();
            let ui = app.ui();
            let pan = crate::zoom::Pan {
                x: ui.get_detail_pan_x() + delta_x,
                y: ui.get_detail_pan_y() + delta_y,
            };
            let viewport = crate::zoom::Viewport::new(
                ui.window().size().width as f32,
                ui.window().size().height as f32,
            );
            let (image_w, image_h) = current_image_size(&ui, &app);
            let scale = ui.get_detail_zoom();
            let clamped = crate::zoom::clamp_pan(pan, viewport, (image_w, image_h), scale);
            ui.set_detail_pan_x(clamped.x);
            ui.set_detail_pan_y(clamped.y);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_request_toggle_zoom(move |_cursor_x, _cursor_y| {
            let app = shared.borrow();
            let ui = app.ui();
            let fitting = ui.get_detail_fit_mode() && (ui.get_detail_zoom() - 1.0).abs() < 0.01;
            ui.set_detail_fit_mode(!fitting);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_reload_image(move || {
            let app = shared.borrow();
            app.reload_detail_page();
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_set_fit_mode(move |fit| {
            let app = shared.borrow();
            app.ui().set_detail_fit_mode(fit);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_set_zoom(move |zoom| {
            let app = shared.borrow();
            app.ui()
                .set_detail_zoom(zoom.clamp(crate::zoom::ZOOM_MIN, crate::zoom::ZOOM_MAX));
        });
    }

    install_capture_fixture(app);
}

/// The app half of `PIXIV_DESKTOP_FIXTURE=ugoira` (the gate half is
/// tools/fixture.ps1).
///
/// It parks the shell in the one state F1 has to be photographed in: the demo data
/// source, the demo catalogue's ugoira open, frame 0 in the viewer and the clock
/// stopped. Nothing here starts playback - the capture walk presses Space on the
/// toolbar button, which is the seam the gate asserts on, and an autoplaying film
/// could not produce two comparable pictures anyway.
fn install_capture_fixture(app: &Shared) {
    if std::env::var("PIXIV_DESKTOP_FIXTURE").ok().as_deref() != Some("ugoira") {
        return;
    }
    {
        let mut app = app.borrow_mut();
        // A fixture must not depend on whether this machine happens to hold a live
        // cookie, so the data source is forced here as well: the capture is of demo
        // works and their pseudo-URLs.
        if app.mode != Mode::Demo {
            app.mode = Mode::Demo;
            app.ui().set_demo_mode(true);
        }
    }
    // The window is not mapped yet, and the deep link (`PIXIV_DESKTOP_START_ILLUST`)
    // is applied after this function runs, so the work is opened once the loop is
    // up - late enough to win over the deep link rather than be overwritten by it.
    let shared = app.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_millis(400),
        move || {
            let id = {
                let app = shared.borrow();
                let mut query = pixiv_types::domain::SearchQuery::new("");
                query.kind = pixiv_types::domain::TypeFilter::Ugoira;
                app.demo.search(&query).items.first().map(|work| work.id)
            };
            if let Some(id) = id {
                App::open_detail(&shared, &id.to_string());
            }
        },
    );
    // The single-shot timer only has to outlive this function.
    std::mem::forget(timer);
}
