use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
    {
        let shared = app.clone();
        ui.on_open_author(move |id| {
            // The author page is native now: no browser hand-off.
            App::open_profile(&shared, id.as_str());
        });
    }

    {
        let shared = app.clone();
        ui.on_open_profile(move |id| App::open_profile(&shared, id.as_str()));
    }

    {
        let shared = app.clone();
        ui.on_profile_back(move || App::travel(&shared, true));
    }

    {
        let shared = app.clone();
        ui.on_profile_refresh(move || App::load_profile(&shared));
    }

    {
        let shared = app.clone();
        ui.on_profile_retry(move || App::load_profile(&shared));
    }

    {
        let shared = app.clone();
        ui.on_profile_follow(move || {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.toggle_profile_follow();
        });
    }

    {
        let shared = app.clone();
        ui.on_profile_load_more(move || App::load_profile_more(&shared));
    }
}
