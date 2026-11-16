//! Novel callbacks: search, the landing page and the reader.
//!
//! Every handler follows the same shape as the rest of the app: borrow the shared
//! app, do the state change, and let the App method do the publishing. Nothing here
//! talks to Slint directly except through those methods, so the page values have
//! exactly one writer.

use super::*;

pub(crate) fn install(app: &Shared) {
    let ui = app.borrow().ui();

    {
        let shared = app.clone();
        ui.on_novel_search(move |word| App::search_novels(&shared, &word, false));
    }
    {
        let shared = app.clone();
        ui.on_novel_load_more(move || {
            let word = shared.borrow().novel.word.clone();
            App::search_novels(&shared, &word, true);
        });
    }
    {
        let shared = app.clone();
        ui.on_novel_refresh(move || {
            let word = shared.borrow().novel.word.clone();
            if word.is_empty() {
                App::load_novels_page(&shared);
            } else {
                App::search_novels(&shared, &word, false);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_novel_open(move |id| App::open_novel_from(&shared, &id));
    }

    // ---- landing page ----
    {
        let shared = app.clone();
        ui.on_novel_read(move || {
            let id = shared.borrow().route.novel_id();
            if let Some(id) = id {
                App::open_reader_route(&shared, id);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_novel_open_chapter(move |id| {
            let Ok(raw) = id.as_str().trim().parse::<u64>() else {
                return;
            };
            App::open_reader_route(&shared, raw);
        });
    }
    {
        let shared = app.clone();
        ui.on_novel_open_tag(move |tag| {
            App::navigate(&shared, Destination::Novels);
            App::search_novels(&shared, &tag, false);
        });
    }
    {
        let shared = app.clone();
        ui.on_novel_open_author(move || {
            let id = {
                let app = shared.borrow();
                app.novel
                    .novel
                    .as_ref()
                    .map(|novel| novel.author.id.get().to_string())
                    .unwrap_or_default()
            };
            App::open_profile(&shared, &id);
        });
    }
    {
        let shared = app.clone();
        ui.on_novel_toggle_bookmark(move || {
            // Novel bookmarks are not implemented: the state is read from the
            // detail document, but the mutation endpoint was never verified live,
            // and a client that guesses a mutation is worse than one that says so.
            shared
                .borrow()
                .notify(2, "小说收藏暂未开放：写入接口尚未验证，作品收藏不受影响");
        });
    }
    {
        let shared = app.clone();
        // Retry reloads whichever novel surface is on screen; the landing page and
        // the reader share one document, so one call serves both.
        ui.on_novel_retry(move || {
            if shared.borrow().page == Destination::NovelReader {
                App::load_reader_page(&shared);
            } else {
                App::load_novel_page(&shared);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_novel_external(move || {
            let id = shared.borrow().route.novel_id().unwrap_or(0);
            if id > 0 {
                let _ = open::that(format!("https://www.pixiv.net/novel/show.php?id={id}"));
            }
        });
    }

    // ---- reader ----
    {
        let shared = app.clone();
        ui.on_reader_scrolled(move |_offset, ratio| {
            if let Ok(mut app) = shared.try_borrow_mut() {
                app.reader_progress(ratio);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_reader_prefs_edit(move |op, value, text| {
            if let Ok(mut app) = shared.try_borrow_mut() {
                app.apply_reader_edit(op, value, text.as_str());
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_reader_back(move || App::travel(&shared, true));
    }
    {
        let shared = app.clone();
        ui.on_reader_chapter(move |delta| {
            let target = shared.borrow().reader_chapter_target(delta);
            if let Some(target) = target {
                App::open_reader_route(&shared, target);
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_reader_toggle_settings(move || {
            if let Ok(mut app) = shared.try_borrow_mut() {
                app.reader_toggle_settings();
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_reader_toggle_chrome(move || {
            if let Ok(mut app) = shared.try_borrow_mut() {
                app.reader_toggle_chrome();
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_reader_open_external(move || {
            let id = shared.borrow().route.novel_id().unwrap_or(0);
            if id > 0 {
                let _ = open::that(format!("https://www.pixiv.net/novel/show.php?id={id}"));
            }
        });
    }
    {
        let shared = app.clone();
        ui.on_reader_retry(move || App::load_reader_page(&shared));
    }
}
