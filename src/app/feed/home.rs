use super::*;

impl App {
    /// Fetch page 1 of whatever the current destination shows, then render.
    pub(crate) fn reload_feed(shared: &Shared) {
        let (page, mode, epoch) = {
            let mut app = shared.borrow_mut();
            app.epoch += 1;
            app.reached_end = false;
            app.page_cursor = 1;
            // A reload is a different set of works: the ids the selection named
            // referred to the page that just went away (F2).
            app.selection_reset();
            // Covers requested for the feed we are leaving no longer belong to
            // any visible page; let the new feed re-plan them.
            app.loader.reset();
            let epoch = app.epoch;
            let ui = app.ui();
            ui.set_grid_state(1);
            ui.set_grid_loading_more(false);
            ui.set_grid_count_label("".into());
            (app.page, app.mode, epoch)
        };

        // Reset the visible page's scroll offset: this is new content.
        {
            let mut app = shared.borrow_mut();
            let ui = app.ui();
            if page == Destination::Home {
                ui.set_home_scroll_y(0.0);
            } else {
                ui.set_grid_scroll_y(0.0);
            }
            let route = app.route;
            app.remember_scroll(route, 0.0);
        }

        // "Search" with no keyword is not a search. It used to reach the API with an
        // empty string (which answered with an unrelated default listing) and, in
        // demo mode, show the whole catalogue under a "搜索" heading. Both read as
        // "these are your results". Prompt for input instead, and offer the recent
        // searches that the app already records in SQLite.
        {
            let mut app = shared.borrow_mut();
            if page == Destination::Search && app.search_word.trim().is_empty() {
                app.feed_owner = Some(app.route);
                app.set_tags(Vec::new());
                let ui = app.ui();
                ui.set_grid_state(2);
                ui.set_grid_state_title("搜索作品".into());
                ui.set_grid_state_glyph("search".into());
                ui.set_grid_message("在上方搜索框输入关键词后按回车。".into());
                ui.set_grid_action_label("".into());
                ui.set_grid_count_label("".into());
                ui.set_grid_columns(ModelRc::default());
                app.columns.clear();
                app.index.clear();
                app.reached_end = true;
                app.refresh_chrome();
                return;
            }
        }

        // History never leaves this machine, so it is answered here rather than
        // in a spawned task. It was previously wired to a stub that returned an
        // empty vector, which is why the page stayed blank even though every
        // opened artwork was recorded in SQLite.
        if page == Destination::History {
            let mut app = shared.borrow_mut();
            // A forced-demo run is self-contained: it must not read the local
            // store, or a screenshot baseline would carry the signed-in user's own
            // browsing history. Measured: the round-five baseline for this page
            // held nine real works and their titles until this line existed.
            let entries = if forced_demo() {
                Vec::new()
            } else {
                app.store.view_history(60).unwrap_or_default()
            };
            let items: Vec<Illust> = entries
                .iter()
                // Rows written by earlier builds in demo mode carry `demo://`
                // covers; they cannot be fetched, so they are dropped rather than
                // rendered as permanently broken cards.
                .filter(|entry| entry.thumb_url.starts_with("http"))
                .map(history_illust)
                .collect();
            app.feed_owner = Some(app.route);
            app.reached_end = true;
            app.set_tags(Vec::new());
            if items.is_empty() {
                let ui = app.ui();
                ui.set_grid_state(2);
                ui.set_grid_state_title("还没有浏览记录".into());
                ui.set_grid_state_glyph("history".into());
                ui.set_grid_message("打开任意作品后，它会按时间出现在这里。".into());
                ui.set_grid_action_label("".into());
                ui.set_grid_count_label("".into());
                ui.set_grid_columns(ModelRc::default());
                app.columns.clear();
                app.index.clear();
            } else {
                app.render_feed(items, false);
            }
            app.refresh_chrome();
            return;
        }

        match mode {
            Mode::Demo => {
                let app = shared.borrow();
                let items = app.demo_items(1);
                let tags = if matches!(page, Destination::Discovery | Destination::Home) {
                    app.demo.trending_tags()
                } else {
                    Vec::new()
                };
                drop(app);
                let mut app = shared.borrow_mut();
                app.feed_owner = Some(app.route);
                app.set_tags(tags);
                app.render_feed(items, false);
                app.refresh_chrome();
            }
            Mode::Real => {
                let app = shared.borrow();
                let client = app.client.clone();
                let rt = app.tasks.clone();
                let word = app.search_word.clone();
                let kind = app.ranking_kind;
                let session = app.session.clone();
                // Round twelve (F1): the request carries the controls as they are
                // *now*, so a reply that lands belongs to the parameters that asked
                // for it even if the user moved the control meanwhile.
                let controls = app.controls;
                drop(app);

                // The history page needs no network at all, so it is answered
                // before a task is spawned.
                rt.spawn(async move {
                    let result = match (client, page) {
                        (Some(client), _) => match page {
                            Destination::Ranking => client
                                .ranking(kind, 1)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect()),
                            Destination::Search => client
                                .search(&controls.query(&word, 1))
                                .await
                                .map(|reply| controls.without_ai(reply.items)),
                            // Never fall back to a default id: user 0 does not
                            // exist, so the request would either 404 or answer
                            // somebody else's page.
                            Destination::Bookmarks => match session.user_id {
                                Some(user) => {
                                    client.bookmarks(user, false, 1).await.map(|r| r.items)
                                }
                                None => Err(PixivError::Unauthorized),
                            },
                            // History is local, so there is nothing to ask
                            // Pixiv. It used to return an empty vector, which made
                            // the page permanently blank even though every opened
                            // artwork was being written to SQLite.
                            Destination::History => Ok(Vec::new()),
                            _ => client
                                .ranking(RankingKind::Daily, 1)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect::<Vec<_>>()),
                        },
                        (None, _) => Err(pixiv_types::PixivError::Api(
                            "没有可用的网络客户端".to_string(),
                        )),
                    };
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.epoch != epoch || app.page != page {
                            // A newer navigation already owns the feed.
                            return;
                        }
                        match result {
                            Ok(items) => {
                                app.feed_owner = Some(app.route);
                                // The related-tag row describes the page that just
                                // arrived, so it is published before the rows are.
                                let tags = if page == Destination::Search {
                                    related_tags(&items, &word, FeedControls::RELATED_LIMIT)
                                } else {
                                    Vec::new()
                                };
                                app.publish_related_tags(tags);
                                app.render_feed(items, false);
                            }
                            Err(error) => app.show_feed_error(&describe_session_error(&error)),
                        }
                        app.refresh_chrome();
                    });
                });
            }
        }
    }
}

impl App {
    pub(crate) fn show_feed_error(&mut self, message: &str) {
        // A failed read has nothing to describe: the related-tag strip belongs to a
        // page that arrived, and a stale one over an error block would offer to
        // search a result set nobody has.
        self.publish_related_tags(Vec::new());
        let ui = self.ui();
        ui.set_grid_state(3);
        ui.set_grid_state_title("载入失败".into());
        ui.set_grid_state_glyph("warning".into());
        ui.set_grid_message(message.into());
        ui.set_grid_action_label("重试".into());
        ui.set_grid_columns(ModelRc::default());
        self.columns.clear();
        self.index.clear();
    }
}

impl App {
    pub(crate) fn set_tags(&self, tags: Vec<String>) {
        let labels: Vec<slint::SharedString> = tags.into_iter().map(Into::into).collect();
        self.ui()
            .set_grid_tags(ModelRc::from(Rc::new(VecModel::from(labels))));
    }
}

impl App {
    pub(crate) fn demo_items(&self, page: u32) -> Vec<Illust> {
        let mut items = self.demo_items_unmarked(page);
        // F4: the catalogue's rows all carry `aiType: 0`, so the AI badge has no
        // work to draw on offline. The fixture marks every third row — a capture
        // state with a name, not a rule the app follows (G24).
        if matches!(
            crate::app::feed::fixture::current(),
            crate::app::feed::fixture::Fixture::Ai
        ) {
            for (index, illust) in items.iter_mut().enumerate() {
                if index % 3 == 0 {
                    illust.ai_type = 1;
                }
            }
        }
        items
    }

    fn demo_items_unmarked(&self, page: u32) -> Vec<Illust> {
        match self.page {
            Destination::Ranking => self
                .demo
                .ranking(self.ranking_kind, page)
                .into_iter()
                .map(|row| row.illust)
                .collect(),
            Destination::Search => {
                self.demo
                    .search(&self.controls.query(&self.search_word, page))
                    .items
            }
            Destination::Bookmarks => self.demo.bookmarks().items,
            _ => {
                let all = self.demo.feed();
                if page <= 1 {
                    all
                } else {
                    // Demo catalogue is finite; page 2 is deliberately empty so
                    // "load more" terminates honestly.
                    Vec::new()
                }
            }
        }
    }
}

/// The page a "load more" asks for, given the page the feed last merged.
pub(crate) fn next_page_cursor(cursor: u32) -> u32 {
    cursor.saturating_add(1)
}

/// Advance the cursor, and only for a page that merged.
///
/// Both halves are one operation on purpose: the cursor used to be advanced
/// before the request, so a page that failed was skipped for good and the next
/// click asked for the one after it (T7, G44).
pub(crate) fn commit_page(cursor: &mut u32, asked: u32, merged: bool) {
    if merged {
        *cursor = asked;
    }
}

impl App {
    /// Record a page the feed really merged (T7).
    pub(crate) fn commit_page_cursor(&mut self, asked: u32) {
        commit_page(&mut self.page_cursor, asked, true);
    }

    pub(crate) fn load_more(shared: &Shared) {
        match shared.borrow().page {
            Destination::Home => {
                App::load_home_more(shared);
                return;
            }
            Destination::Discovery => {
                App::load_discovery(shared, true);
                return;
            }
            Destination::UserProfile => {
                App::load_profile_more(shared);
                return;
            }
            _ => {}
        }
        let should_load = {
            let mut app = shared.borrow_mut();
            if app.loading_more || app.reached_end || app.page == Destination::Detail {
                return;
            }
            app.loading_more = true;
            // Nothing moves here: the page is only the feed's once it merges (T7).
            app.ui().set_grid_loading_more(true);
            true
        };
        if !should_load {
            return;
        }

        let (page, mode, cursor, epoch) = {
            let app = shared.borrow();
            (
                app.page,
                app.mode,
                next_page_cursor(app.page_cursor),
                app.epoch,
            )
        };

        match mode {
            Mode::Demo => {
                let mut app = shared.borrow_mut();
                let next = app.demo_items(cursor);
                app.loading_more = false;
                let ui = app.ui();
                ui.set_grid_loading_more(false);
                if next.is_empty() {
                    app.reached_end = true;
                    return;
                }
                app.commit_page_cursor(cursor);
                app.render_feed(next, true);
            }
            Mode::Real => {
                let app = shared.borrow();
                let client = app.client.clone();
                let rt = app.tasks.clone();
                let word = app.search_word.clone();
                let kind = app.ranking_kind;
                let session = app.session.clone();
                let controls = app.controls;
                drop(app);
                rt.spawn(async move {
                    let result = match client {
                        Some(client) => match page {
                            Destination::Ranking => client
                                .ranking(kind, cursor)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect()),
                            Destination::Search => client
                                .search(&controls.query(&word, cursor))
                                .await
                                .map(|reply| controls.without_ai(reply.items)),
                            Destination::Bookmarks => match session.user_id {
                                Some(user) => {
                                    client.bookmarks(user, false, cursor).await.map(|r| r.items)
                                }
                                None => Err(PixivError::Unauthorized),
                            },
                            // A local list has no next page, and falling
                            // through to the daily ranking would have appended
                            // unrelated live works to the history.
                            Destination::History => Ok(Vec::new()),
                            _ => client
                                .ranking(RankingKind::Daily, cursor)
                                .await
                                .map(|rows| rows.into_iter().map(|r| r.illust).collect::<Vec<_>>()),
                        },
                        None => Ok(Vec::new()),
                    };
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        app.loading_more = false;
                        if app.epoch != epoch {
                            return;
                        }
                        let ui = app.ui();
                        ui.set_grid_loading_more(false);
                        match result {
                            Ok(items) if !items.is_empty() => {
                                // The page arrived: only now does the feed own it, so a
                                // failure below leaves the cursor on the page that
                                // failed instead of skipping it (T7).
                                app.commit_page_cursor(cursor);
                                app.render_feed(items, true);
                            }
                            Ok(_) => app.reached_end = true,
                            Err(error) => ui.set_status_text(
                                format!("加载更多失败：{}", describe_session_error(&error)).into(),
                            ),
                        }
                    });
                });
            }
        }
    }
}

impl App {
    // ------------------------------------------------------------- home

    /// Load all four home bands.
    ///
    /// The daily ranking is not consulted here at all: it is a different page
    /// with a different meaning. Using it as a fallback is what made the home
    /// page indistinguishable from 排行榜.
    pub(crate) fn load_home(shared: &Shared) {
        let (mode, epoch, client, rt, route) = {
            let mut app = shared.borrow_mut();
            app.epoch += 1;
            app.reached_end = false;
            app.page_cursor = 1;
            app.loading_more = false;
            app.loader.reset();
            app.home = HomeBands::loading();
            app.feed_owner = None;
            app.selection_reset();
            app.items.clear();
            app.columns.clear();
            app.index.clear();
            let epoch = app.epoch;
            {
                let ui = app.ui();
                ui.set_home_scroll_y(0.0);
                ui.set_grid_scroll_y(0.0);
                ui.set_grid_state(BAND_LOADING);
                ui.set_grid_columns(ModelRc::default());
                ui.set_grid_loading_more(false);
                ui.set_grid_count_label("".into());
            }
            let route = app.route;
            app.remember_scroll(route, 0.0);
            app.publish_home();
            (
                app.mode,
                epoch,
                app.client.clone(),
                app.tasks.clone(),
                app.route,
            )
        };

        if mode == Mode::Demo {
            let app = shared.borrow();
            let items = app.demo_items(1);
            let tags: Vec<RecommendTag> = app
                .demo
                .trending_tags()
                .into_iter()
                .map(|name| RecommendTag {
                    translated: None,
                    name,
                    emphasized: false,
                })
                .collect();
            drop(app);
            let mut app = shared.borrow_mut();
            app.home.works_state = BAND_OK;
            app.home.tags_state = if tags.is_empty() { BAND_EMPTY } else { BAND_OK };
            app.home.tags = tags;
            app.home.latest_state = BAND_EMPTY;
            app.home.latest_message = "演示模式不提供真实最新作品".to_string();
            app.home.artists_state = BAND_EMPTY;
            app.home.artists_message = "演示模式不提供推荐画师".to_string();
            app.feed_owner = Some(route);
            app.render_feed(items, false);
            app.publish_home();
            app.refresh_chrome();
            return;
        }

        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.fail_home("没有可用的网络客户端");
            app.refresh_chrome();
            return;
        };

        rt.spawn(async move {
            let (a, b, c, d) = (
                client.clone(),
                client.clone(),
                client.clone(),
                client.clone(),
            );
            let (works, tags, artists, latest) = tokio::join!(
                a.home_feed(),
                b.recommend_tags(),
                c.recommend_users(),
                d.latest_works()
            );
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                if app.epoch != epoch || app.page != Destination::Home {
                    return;
                }
                let mut avatar_urls: Vec<String> = Vec::new();

                match works {
                    Ok(feed) => {
                        app.home.works = feed.works.clone();
                        app.home.works_state = if feed.works.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                        avatar_urls
                            .extend(feed.works.iter().filter_map(|w| w.author.avatar.clone()));
                    }
                    Err(error) => {
                        let message = describe_session_error(&error);
                        app.home.works_state = BAND_ERROR;
                        app.home.works_message = message.clone();
                        app.show_feed_error(&message);
                    }
                }

                match tags {
                    Ok(tags) => {
                        app.home.tags = tags;
                        app.home.tags_state = if app.home.tags.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                    }
                    Err(error) => {
                        app.home.tags_state = BAND_ERROR;
                        app.home.tags_message = describe_session_error(&error);
                    }
                }

                match artists {
                    Ok(artists) => {
                        app.home.artists = artists;
                        app.home.artists_state = if app.home.artists.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                        avatar_urls.extend(App::user_image_urls(&app.home.artists));
                    }
                    Err(error) => {
                        app.home.artists_state = BAND_ERROR;
                        app.home.artists_message = describe_session_error(&error);
                    }
                }

                match latest {
                    Ok(latest) => {
                        app.home.latest_title = latest.title.clone();
                        app.home.latest = latest.works.clone();
                        app.home.latest_state = if latest.works.is_empty() {
                            BAND_EMPTY
                        } else {
                            BAND_OK
                        };
                        avatar_urls
                            .extend(latest.works.iter().filter_map(|w| w.author.avatar.clone()));
                    }
                    Err(error) => {
                        app.home.latest_state = BAND_ERROR;
                        app.home.latest_message = describe_session_error(&error);
                    }
                }

                // Refresh the suggestion panel's focus-state rows from the tags
                // that just arrived: these are the server's own picks.
                app.suggest.base = app
                    .home
                    .tags
                    .iter()
                    .map(|tag| Suggestion {
                        tag: tag.name.clone(),
                        translated: tag.translated.clone(),
                        illust_count: None,
                        popularity: None,
                        source: pixiv_types::domain::SuggestionSource::Pixiv,
                    })
                    .collect();

                app.feed_owner = Some(app.route);
                let works = app.home.works.clone();
                app.render_feed(works, false);
                app.publish_home();
                app.spawn_avatars(avatar_urls);
                app.refresh_chrome();
            });
        });
    }
}

impl App {
    /// Append another for-you batch. The street feed is not paginated, so a
    /// second call is simply a second sample of the same recommendation engine.
    pub(crate) fn load_home_more(shared: &Shared) {
        let (client, rt, epoch, reached_end) = {
            let mut app = shared.borrow_mut();
            if app.loading_more || app.reached_end || app.home.exhausted {
                return;
            }
            app.loading_more = true;
            app.ui().set_grid_loading_more(true);
            (
                app.client.clone(),
                app.tasks.clone(),
                app.epoch,
                app.reached_end,
            )
        };
        let _ = reached_end;
        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.reached_end = true;
            return;
        };
        rt.spawn(async move {
            let feed = client.home_feed().await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.loading_more = false;
                app.ui().set_grid_loading_more(false);
                if app.epoch != epoch || app.page != Destination::Home {
                    return;
                }
                match feed {
                    Ok(feed) => {
                        let urls: Vec<String> = feed
                            .works
                            .iter()
                            .filter_map(|w| w.author.avatar.clone())
                            .collect();
                        // Only the rows this batch added. The whole accumulated
                        // list used to be cloned and handed over with append =
                        // false, which bought nothing (the clone) and disabled the
                        // extend path (the flag) - plan §2.3 R11.
                        let batch = merge_page(&mut app.home.works, feed.works);
                        let added = batch.len();
                        app.home.batches += 1;
                        if added == 0 {
                            app.home.empty_batches += 1;
                        } else {
                            app.home.empty_batches = 0;
                        }
                        if app.home.empty_batches >= 2 || app.home.batches >= 8 {
                            app.home.exhausted = true;
                            app.reached_end = true;
                        }
                        if added > 0 {
                            // The batch, appended. `self.items` already holds the
                            // rest of the grid, so the clone of the whole list is
                            // gone and only the new rows are laid out.
                            app.render_feed(batch, true);
                        }
                        app.spawn_avatars(urls);
                        app.publish_home();
                    }
                    Err(error) => {
                        app.ui()
                            .set_status_text(format!("加载更多失败：{error}").into());
                        app.reached_end = true;
                    }
                }
            });
        });
    }
}

impl App {
    pub(crate) fn fail_home(&mut self, message: &str) {
        self.show_feed_error(message);
        self.home.works_state = BAND_ERROR;
        self.home.works_message = message.to_string();
        self.home.tags_state = BAND_ERROR;
        self.home.tags_message = message.to_string();
        self.home.artists_state = BAND_ERROR;
        self.home.artists_message = message.to_string();
        self.home.latest_state = BAND_ERROR;
        self.home.latest_message = message.to_string();
        self.publish_home();
    }
}

impl App {
    /// Push the home bands into the window.
    pub(crate) fn publish_home(&self) {
        let ui = self.ui();
        let tags: Vec<RecommendTagItem> = self
            .home
            .tags
            .iter()
            .map(|tag| RecommendTagItem {
                name: tag.name.clone().into(),
                label: tag.display().to_string().into(),
                emphasized: tag.emphasized,
            })
            .collect();
        ui.set_home_tags(ModelRc::from(Rc::new(VecModel::from(tags))));

        let artists: Vec<RecommendUserItem> = self
            .home
            .artists
            .iter()
            .map(|user| self.user_item(user))
            .collect();
        ui.set_home_artists(ModelRc::from(Rc::new(VecModel::from(artists))));

        let latest: Vec<IllustItem> = self
            .home
            .latest
            .iter()
            .map(|illust| self.illust_item_avatar(illust, 0))
            .collect();
        ui.set_home_latest(ModelRc::from(Rc::new(VecModel::from(latest))));

        ui.set_home_latest_title(if self.home.latest_title.trim().is_empty() {
            "最新作品".into()
        } else {
            self.home.latest_title.clone().into()
        });
        ui.set_home_feed_state(self.home.works_state);
        ui.set_home_latest_state(self.home.latest_state);
        ui.set_home_tags_state(self.home.tags_state);
        ui.set_home_artists_state(self.home.artists_state);
        ui.set_home_feed_message(self.home.works_message.clone().into());
        ui.set_home_latest_message(self.home.latest_message.clone().into());
        ui.set_home_tags_message(self.home.tags_message.clone().into());
        ui.set_home_artists_message(self.home.artists_message.clone().into());

        let note = match self.mode {
            Mode::Demo => "本地演示作品",
            Mode::Real if !self.session.is_authenticated() => "未登录 · 公开推荐",
            Mode::Real => "",
        };
        ui.set_home_source_note(note.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G44 - the page cursor only advances on success.
    ///
    /// The cursor used to be incremented before the request went out, so a failed
    /// "load more" skipped its page for good: the next click asked for the one after
    /// it, and the rows between were never shown. The assertion that fails on the old
    /// tree is the middle one - the second request asked for page 3.
    #[test]
    fn g44_a_failed_load_more_asks_for_the_same_page_again() {
        let mut cursor = 1;
        let asked = next_page_cursor(cursor);
        assert_eq!(asked, 2, "the first load-more asks for page 2");

        // The page failed: nothing merged, so nothing moves.
        commit_page(&mut cursor, asked, false);
        assert_eq!(
            next_page_cursor(cursor),
            2,
            "the page that failed is asked for again, not skipped"
        );

        // The same page this time answered.
        commit_page(&mut cursor, asked, true);
        assert_eq!(
            next_page_cursor(cursor),
            3,
            "a page that merged is never asked for twice"
        );
    }
}
