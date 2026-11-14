use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
    {
        let shared = app.clone();
        ui.on_width_changed(move |width| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.on_window_width(width);
        });
    }

    {
        let shared = app.clone();
        ui.on_navigate(move |page| App::navigate(&shared, page));
    }

    {
        let shared = app.clone();
        ui.on_go_back(move || App::travel(&shared, true));
    }

    {
        let shared = app.clone();
        ui.on_go_forward(move || App::travel(&shared, false));
    }

    {
        let shared = app.clone();
        ui.on_shell_shortcut(move |action| match action.as_str() {
            "escape" => {
                let (page, suggesting) = {
                    let app = shared.borrow();
                    (app.page, app.suggest.visible)
                };
                if suggesting {
                    shared.borrow_mut().suggest_close();
                } else if page == Destination::Detail {
                    App::travel(&shared, true);
                }
            }
            // An explicit browser hand-off is still available, but it is never
            // the default for an author link.
            "open-external" => {
                let target = {
                    let app = shared.borrow();
                    app.profile.as_ref().map(|state| state.user.get())
                };
                if let Some(uid) = target {
                    let _ = open::that(format!("https://www.pixiv.net/users/{uid}"));
                }
            }
            _ => {}
        });
    }

    {
        let shared = app.clone();
        ui.on_toggle_fullscreen(move || {
            let app = shared.borrow();
            let window = app.ui();
            let window = window.window();
            window.set_fullscreen(!window.is_fullscreen());
        });
    }

    {
        let shared = app.clone();
        ui.on_dismiss_notice(move || {
            let app = shared.borrow();
            app.ui().set_notice("".into());
        });
    }
}
