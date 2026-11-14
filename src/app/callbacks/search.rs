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
            if word.is_empty() {
                let app = shared.borrow();
                app.notify(2, "请输入关键词");
                return;
            }
            {
                let mut app = shared.borrow_mut();
                app.suggest_close();
                app.search_word = word.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&word);
                // The list the empty state renders has to include what was just
                // searched for; `refresh_chrome` runs again on navigate but the
                // store write and the read are ordered here so the chip appears
                // without a second navigation.
                app.refresh_recent_searches();
                app.ui().set_search_text(word.clone().into());
            }
            App::navigate(&shared, Destination::Search);
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
            let tag = tag.to_string();
            {
                let mut app = shared.borrow_mut();
                app.suggest_close();
                app.search_word = tag.clone();
                app.feed_owner = None;
                let _ = app.store.push_search(&tag);
                app.refresh_recent_searches();
                app.ui().set_search_text(tag.clone().into());
            }
            App::navigate(&shared, Destination::Search);
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
