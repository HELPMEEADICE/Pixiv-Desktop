use super::*;

pub(crate) struct DiscoveryBands {
    pub(crate) users: Vec<RecommendUser>,
    pub(crate) users_state: i32,
    pub(crate) users_message: String,
    /// Every id this page has already rendered or tried, so "load more" cannot
    /// append the same work twice.
    pub(crate) seen: HashSet<u64>,
    pub(crate) batches: u32,
    pub(crate) empty_batches: u32,
    pub(crate) exhausted: bool,
}

impl Default for DiscoveryBands {
    fn default() -> Self {
        Self {
            users: Vec::new(),
            users_state: BAND_LOADING,
            users_message: String::new(),
            seen: HashSet::new(),
            batches: 0,
            empty_batches: 0,
            exhausted: false,
        }
    }
}

impl App {
    // ------------------------------------------------------------- discovery

    /// Load one discovery batch.
    ///
    /// The discovery engine is not paginated: every call returns a fresh ranked
    /// batch, so "load more" is a new request plus de-duplication against
    /// everything already shown, and the loader stops on its own.
    pub(crate) fn load_discovery(shared: &Shared, append: bool) {
        let (mode, epoch, client, rt, seen) = {
            let mut app = shared.borrow_mut();
            if append {
                if app.loading_more || app.reached_end || app.discovery.exhausted {
                    return;
                }
                app.loading_more = true;
                app.ui().set_grid_loading_more(true);
            } else {
                app.epoch += 1;
                app.reached_end = false;
                app.page_cursor = 1;
                app.loader.reset();
                app.discovery = DiscoveryBands::default();
                app.feed_owner = None;
                app.items.clear();
                app.columns.clear();
                app.index.clear();
                {
                    let ui = app.ui();
                    ui.set_grid_scroll_y(0.0);
                    ui.set_grid_state(BAND_LOADING);
                    ui.set_grid_columns(ModelRc::default());
                    ui.set_grid_loading_more(false);
                    ui.set_grid_count_label("".into());
                }
                app.publish_discovery_users();
            }
            (
                app.mode,
                app.epoch,
                app.client.clone(),
                app.rt.clone(),
                app.discovery.seen.clone(),
            )
        };

        if mode == Mode::Demo {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.discovery.users_state = BAND_EMPTY;
            app.discovery.users_message = "演示模式不提供推荐画师".to_string();
            app.discovery.exhausted = true;
            let cursor = if append { app.page_cursor + 1 } else { 1 };
            app.page_cursor = cursor;
            let items = app.demo_items(cursor);
            if items.is_empty() {
                app.reached_end = true;
            } else {
                app.feed_owner = Some(app.route);
                app.render_feed(items, append);
            }
            app.publish_discovery_users();
            app.refresh_chrome();
            return;
        }

        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.show_feed_error("没有可用的网络客户端");
            app.refresh_chrome();
            return;
        };

        rt.spawn(async move {
            // The batch route names sixty recommendations but describes only a
            // handful, and the discovery route describes ten in full. Both are
            // the same engine, so asking for both fills a screen immediately and
            // still leaves ids to hydrate.
            let (quick_client, pool_client, user_client) =
                (client.clone(), client.clone(), client.clone());
            let (quick, pool, users) = tokio::join!(
                quick_client.illust_discovery("all"),
                pool_client.discovery_artworks("all", 60),
                user_client.discovery_users("all")
            );

            let mut works: Vec<Illust> = Vec::new();
            let mut pending: Vec<Id> = Vec::new();
            let mut quick_error: Option<String> = None;
            match quick {
                Ok(rows) => works.extend(rows),
                Err(error) => quick_error = Some(describe_session_error(&error)),
            }
            if let Ok(batch) = &pool {
                works.extend(batch.works.iter().cloned());
                pending.extend(batch.pending.iter().copied());
            }

            // Hydrate the next few recommended ids, skipping anything this page
            // has already shown.
            let mut targets: Vec<Id> = Vec::new();
            for id in &pending {
                if targets.len() >= DISCOVERY_HYDRATE || seen.contains(&id.get()) {
                    continue;
                }
                targets.push(*id);
            }
            let mut hydrated: Vec<Illust> = Vec::new();
            if !targets.is_empty() {
                let detail_client = client.clone();
                let mut stream = futures::stream::iter(targets)
                    .map(move |id| {
                        let client = detail_client.clone();
                        async move {
                            match client.illust_detail(id).await {
                                Ok(illust) => Some(illust),
                                Err(error) => {
                                    // One work fewer on the grid is not worth a page
                                    // state, but it is worth a line (G41).
                                    tracing::debug!(%error, id = id.get(), "discovery hydrate failed");
                                    None
                                }
                            }
                        }
                    })
                    .buffer_unordered(4);
                while let Some(found) = stream.next().await {
                    if let Some(illust) = found {
                        hydrated.push(illust);
                    }
                }
            }
            works.extend(hydrated);

            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.loading_more = false;
                app.ui().set_grid_loading_more(false);
                if app.epoch != epoch || app.page != Destination::Discovery {
                    return;
                }

                if let Some(message) = quick_error
                    && app.items.is_empty()
                    && works.is_empty()
                {
                    app.show_feed_error(&message);
                    app.discovery.exhausted = true;
                    app.refresh_chrome();
                    return;
                }

                let mut urls: Vec<String> = works
                    .iter()
                    .filter_map(|illust| illust.author.avatar.clone())
                    .collect();

                if let Ok(users) = users {
                    app.discovery.users = users;
                    app.discovery.users_state = if app.discovery.users.is_empty() {
                        BAND_EMPTY
                    } else {
                        BAND_OK
                    };
                    // The artists' own faces and previews are not in the work
                    // batch, so they are requested here explicitly.
                    urls.extend(App::user_image_urls(&app.discovery.users));
                } else if app.discovery.users.is_empty() {
                    app.discovery.users_state = BAND_ERROR;
                    app.discovery.users_message = "推荐画师载入失败".to_string();
                }

                // Record every id this batch touched so the next one cannot
                // re-add it, including the ones that failed to hydrate.
                for illust in &works {
                    app.discovery.seen.insert(illust.id.get());
                }
                for id in &pending {
                    app.discovery.seen.insert(id.get());
                }
                let added = pixiv_types::domain::append_unique(
                    &mut app.items,
                    works.into_iter().filter(|illust| !illust.id.is_zero()),
                );

                app.discovery.batches += 1;
                if added == 0 {
                    app.discovery.empty_batches += 1;
                } else {
                    app.discovery.empty_batches = 0;
                }
                if app.discovery.empty_batches >= 2
                    || app.discovery.batches >= DISCOVERY_MAX_BATCHES
                {
                    app.discovery.exhausted = true;
                    app.reached_end = true;
                }

                app.feed_owner = Some(app.route);
                if added == 0 && app.items.is_empty() {
                    app.ui().set_grid_state(BAND_EMPTY);
                    app.ui().set_grid_state_title("没有更多推荐".into());
                    app.ui().set_grid_state_glyph("discover".into());
                    app.ui()
                        .set_grid_message("Pixiv 的推荐引擎这次没有返回新作品，稍后再试。".into());
                    app.ui().set_grid_action_label("".into());
                    app.ui().set_grid_columns(ModelRc::default());
                    app.columns.clear();
                    app.index.clear();
                } else {
                    app.render_feed(Vec::new(), true);
                }
                app.publish_discovery_users();
                app.spawn_avatars(urls);
                app.refresh_chrome();
            });
        });
    }
}

impl App {
    pub(crate) fn publish_discovery_users(&self) {
        let ui = self.ui();
        let users: Vec<RecommendUserItem> = self
            .discovery
            .users
            .iter()
            .map(|user| self.user_item(user))
            .collect();
        ui.set_discovery_users(ModelRc::from(Rc::new(VecModel::from(users))));
        ui.set_discovery_users_state(self.discovery.users_state);
        ui.set_discovery_users_message(self.discovery.users_message.clone().into());
    }
}
