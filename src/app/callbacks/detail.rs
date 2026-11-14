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
            let app = shared.borrow();
            let ui = app.ui();
            ui.set_detail_page_index(index);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
            ui.set_detail_fit_mode(true);
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
}
