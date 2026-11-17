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
    //
    // Every arm below takes the viewport from the *event*, never from
    // `ui.window().size()`. The window is not the surface the picture is drawn on:
    // it also holds the navigation rail, the toolbar, the info panel and the
    // thumbnail strip, so clamping against it let a drag travel hundreds of pixels
    // past the edges of the clipped viewer and then refused to come back.
    {
        let shared = app.clone();
        ui.on_detail_request_zoom(move |cursor_x, cursor_y, delta_y, view_w, view_h| {
            let app = shared.borrow();
            let ui = app.ui();
            let viewport = crate::zoom::Viewport::new(view_w, view_h);
            let current = ui.get_detail_zoom();
            let next = crate::zoom::wheel_zoom(current, delta_y);
            let scale_changed = (next - current).abs() > f32::EPSILON;
            let changed =
                scale_changed || (ui.get_detail_pan_x() != 0.0 || ui.get_detail_pan_y() != 0.0);
            if !changed {
                return;
            }
            let pan = crate::zoom::Pan {
                x: ui.get_detail_pan_x(),
                y: ui.get_detail_pan_y(),
            };
            let moved = if scale_changed {
                crate::zoom::anchored_pan(pan, (cursor_x, cursor_y), viewport, next / current)
            } else {
                pan
            };
            let (image_w, image_h) = current_image_size(&ui, &app);
            let out = crate::zoom::resolve(
                viewport,
                (image_w, image_h),
                ui.get_detail_fit_mode(),
                next,
                moved,
            );
            ui.set_detail_zoom(next);
            ui.set_detail_pan_x(out.pan.x);
            ui.set_detail_pan_y(out.pan.y);
            drop(app);
            remember_viewport(&shared, viewport);
        });
    }

    {
        let shared = app.clone();
        ui.on_detail_request_pan(move |delta_x, delta_y, view_w, view_h| {
            let app = shared.borrow();
            let ui = app.ui();
            let viewport = crate::zoom::Viewport::new(view_w, view_h);
            let (image_w, image_h) = current_image_size(&ui, &app);
            let pan = crate::zoom::Pan {
                x: ui.get_detail_pan_x() + delta_x,
                y: ui.get_detail_pan_y() + delta_y,
            };
            // The scale the image is *drawn* at, which in fit mode is the fit scale
            // times the multiplier. Reading the raw multiplier here clamped the pan
            // against an image a fraction of its real size, so a fitted picture could
            // be dragged far past its own edges.
            let out = crate::zoom::resolve(
                viewport,
                (image_w, image_h),
                ui.get_detail_fit_mode(),
                ui.get_detail_zoom(),
                pan,
            );
            ui.set_detail_pan_x(out.pan.x);
            ui.set_detail_pan_y(out.pan.y);
            drop(app);
            remember_viewport(&shared, viewport);
        });
    }

    // Double-click: WinUI's media idiom is "toggle between fit and 1:1, keeping the
    // point you clicked under the cursor". The old arm ignored the cursor and reset
    // the pan to the centre, so the picture jumped under the pointer on every
    // double-click.
    {
        let shared = app.clone();
        ui.on_detail_request_toggle_zoom(move |cursor_x, cursor_y| {
            let app = shared.borrow();
            let ui = app.ui();
            // The viewer box the click landed in. Double-click does not carry it, so
            // it is the last geometry the pointer reported - the value Rust keeps for
            // exactly this kind of gesture.
            let viewport = crate::zoom::Viewport::new(app.viewer_viewport.0, app.viewer_viewport.1);
            let (image_w, image_h) = current_image_size(&ui, &app);
            let fit_mode = ui.get_detail_fit_mode();
            let current = ui.get_detail_zoom();
            // "Fitted" means fit mode is on *and* the multiplier has not been wheeled
            // away from 1.0; only then is the next double-click a zoom to 1:1.
            let fitted = fit_mode && (current - 1.0).abs() < 0.01;
            let (target_fit, next) = crate::zoom::toggled_mode(fitted);
            let pan = crate::zoom::Pan {
                x: ui.get_detail_pan_x(),
                y: ui.get_detail_pan_y(),
            };
            let moved = if viewport.is_usable() && current > 0.0 {
                crate::zoom::anchored_pan(pan, (cursor_x, cursor_y), viewport, next / current)
            } else {
                crate::zoom::Pan::default()
            };
            let out = crate::zoom::resolve(viewport, (image_w, image_h), target_fit, next, moved);
            ui.set_detail_fit_mode(target_fit);
            ui.set_detail_zoom(next);
            ui.set_detail_pan_x(out.pan.x);
            ui.set_detail_pan_y(out.pan.y);
        });
    }

    // The toolbar's "actual size" button. It reaches 1:1 in *both* modes: in fit mode
    // the multiplier rides on top of the fit scale, so the old `set-zoom(1.0)`
    // reported "actual size" while drawing a 4000px scan at 25%.
    {
        let shared = app.clone();
        ui.on_detail_set_actual_size(move || {
            let app = shared.borrow();
            let ui = app.ui();
            // Both halves of the mode, from the one place that defines them.
            let (fit, zoom) = crate::zoom::toggled_mode(ui.get_detail_fit_mode());
            ui.set_detail_fit_mode(fit);
            ui.set_detail_zoom(zoom);
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

    // Switching mode alone is not a complete state change: the multiplier means
    // something different in each mode, so the pair is written together and the pan is
    // dropped back to centre. Every caller that used to follow this with
    // `set-zoom(1.0)` now gets the same result from one signal.
    {
        let shared = app.clone();
        ui.on_detail_set_fit_mode(move |fit| {
            let app = shared.borrow();
            let ui = app.ui();
            if ui.get_detail_fit_mode() == fit && (ui.get_detail_zoom() - 1.0).abs() < f32::EPSILON
            {
                return;
            }
            let (_, zoom) = crate::zoom::toggled_mode(!fit);
            ui.set_detail_fit_mode(fit);
            ui.set_detail_zoom(zoom);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
        });
    }

    // The +/- buttons. They used to write the multiplier and nothing else, so the pan
    // left over from a previous scale stayed where it was and could sit outside the
    // image. Resolving through the same geometry the wheel uses keeps the two in step:
    // a button press and a wheel notch produce the same state for the same scale.
    {
        let shared = app.clone();
        ui.on_detail_set_zoom(move |zoom| {
            let app = shared.borrow();
            let ui = app.ui();
            let zoom = if zoom.is_finite() {
                zoom.clamp(crate::zoom::ZOOM_MIN, crate::zoom::ZOOM_MAX)
            } else {
                1.0
            };
            let viewport = crate::zoom::Viewport::new(app.viewer_viewport.0, app.viewer_viewport.1);
            if !viewport.is_usable() {
                ui.set_detail_zoom(zoom);
                return;
            }
            let (image_w, image_h) = current_image_size(&ui, &app);
            let out = crate::zoom::resolve(
                viewport,
                (image_w, image_h),
                ui.get_detail_fit_mode(),
                zoom,
                crate::zoom::Pan {
                    x: ui.get_detail_pan_x(),
                    y: ui.get_detail_pan_y(),
                },
            );
            ui.set_detail_zoom(zoom);
            ui.set_detail_pan_x(out.pan.x);
            ui.set_detail_pan_y(out.pan.y);
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
/// Remember the viewer box the pointer last reported.
///
/// Double-click and the toolbar's zoom buttons carry no geometry of their own, so
/// they use this. It is written by the wheel and the drag - the two gestures that do
/// know the box - and it is the only stored viewport in the app.
fn remember_viewport(shared: &Shared, viewport: crate::zoom::Viewport) {
    if !viewport.is_usable() {
        return;
    }
    if let Ok(mut app) = shared.try_borrow_mut() {
        app.viewer_viewport = (viewport.width, viewport.height);
    }
}

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
