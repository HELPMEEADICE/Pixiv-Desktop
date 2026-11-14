use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();
    {
        let shared = app.clone();
        ui.on_search_submitted(move |word| {
            // The highlighted suggestion wins over the raw text: that is what
            // pressing Enter on a dropdown row means everywhere else.
            let word = {
                let app = shared.borrow();
                app.suggest_resolve(word.as_str()).trim().to_string()
            };
            App::submit_search(&shared, &word);
        });
    }

    {
        let shared = app.clone();
        ui.on_search_edited(move |text| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.suggest_query(&shared, text.as_str());
        });
    }

    {
        let shared = app.clone();
        ui.on_suggest_key(move |key| {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.suggest_move(key.as_str());
        });
    }

    {
        let shared = app.clone();
        ui.on_suggest_chosen(move |tag| {
            // One implementation of "run this word", shared with the palette.
            App::submit_search(&shared, tag.as_str());
        });
    }

    {
        let shared = app.clone();
        ui.on_suggest_dismissed(move || {
            let Ok(mut app) = shared.try_borrow_mut() else {
                return;
            };
            app.suggest_close();
        });
    }
}
