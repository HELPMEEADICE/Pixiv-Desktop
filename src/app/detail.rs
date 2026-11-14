use super::*;

impl App {
    // ------------------------------------------------------------- detail

    /// Open a work from anywhere in the UI.
    pub(crate) fn open_detail(shared: &Shared, id: &str) {
        let Ok(raw) = id.trim().parse::<u64>() else {
            return;
        };
        App::open_detail_route(shared, raw, true);
    }
}

impl App {
    /// Hand one view to the store without blocking the frame.
    ///
    /// The queue is bounded and single-consumed: rows are written in the order they
    /// were opened, because a history page ordered by anything else would be a
    /// different history. A full queue means the database is 64 rows behind and the
    /// newest view is dropped - `record_view` is an upsert by illustration id, so the
    /// next view of the same work still lands (plan §2.3 R16).
    pub(crate) fn record_view(&self, entry: HistoryEntry) {
        if self.view_queue.try_send(entry).is_err() {
            tracing::debug!("view-history queue is full; dropping one row");
        }
    }
}

impl App {
    /// Load one work into the detail page.
    ///
    /// push_history is false when the route is already on the stack (Back and
    /// Forward restore a destination; they do not create a new one).
    pub(crate) fn open_detail_route(shared: &Shared, raw: u64, push_history: bool) {
        let illust_id = Id(raw);

        let (mode, epoch) = {
            let mut app = shared.borrow_mut();
            let current = app.route;
            let target = Route::illust(Destination::Detail, raw);
            if push_history {
                app.history.visit(current, target);
                let scroll = app.current_scroll();
                app.remember_scroll(current, scroll);
            }
            app.route = target;
            app.page = Destination::Detail;
            app.detail_epoch += 1;
            let epoch = app.detail_epoch;
            // Opening a work leaves the page behind it exactly the way a fresh
            // navigation does, tooltip included: this is the third exit path and it
            // had the same missing reset (plan §2.3 R20).
            app.leave_page(target);
            let ui = app.ui();
            ui.set_detail_state(1);
            ui.set_detail_pages(ModelRc::default());
            ui.set_detail_page_index(0);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
            ui.set_detail_fit_mode(true);
            ui.set_detail_busy(false);
            ui.set_detail_following(false);
            ui.set_detail_ugoira_hint("".into());
            ui.set_detail_tags(ModelRc::default());
            (app.mode, epoch)
        };

        match mode {
            Mode::Demo => {
                let app = shared.borrow();
                let found = app.demo.detail(illust_id);
                let pages = app.demo.pages(illust_id);
                drop(app);
                let Some(illust) = found else {
                    App::fail_detail(shared, "找不到该作品，或它对该账号不可见。");
                    return;
                };
                let mut app = shared.borrow_mut();
                app.install_detail(illust, pages, epoch);
            }
            Mode::Real => {
                let app = shared.borrow();
                let client = app.client.clone();
                let rt = app.rt.clone();
                drop(app);
                let Some(client) = client else {
                    App::fail_detail(shared, "没有可用的网络客户端。");
                    return;
                };
                rt.spawn(async move {
                    // Detail, pages and follow-state in one round trip batch: the
                    // page used to show a single page for every real artwork
                    // because the page list was never fetched.
                    let detail = client.illust_detail(illust_id).await;
                    let (illust, pages, following) = match detail {
                        Ok(illust) => {
                            let pages = client
                                .illust_pages(illust_id)
                                .await
                                .unwrap_or_else(|_| Vec::new());
                            let following = client
                                .user_profile(illust.author.id)
                                .await
                                .map(|profile| profile.is_followed)
                                .unwrap_or(false);
                            (illust, pages, following)
                        }
                        Err(error) => {
                            on_ui(move |shared| {
                                App::fail_detail(shared, &error.to_string());
                            });
                            return;
                        }
                    };
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.detail_epoch != epoch {
                            return;
                        }
                        app.install_detail_with_follow(illust, pages, following, epoch);
                    });
                });
            }
        }
    }
}

impl App {
    pub(crate) fn fail_detail(shared: &Shared, message: &str) {
        let app = shared.borrow();
        let ui = app.ui();
        ui.set_detail_state(3);
        ui.set_detail_message(message.into());
    }
}

impl App {
    pub(crate) fn install_detail(&mut self, illust: Illust, pages: Vec<PageMeta>, epoch: u64) {
        let following = false;
        self.install_detail_with_follow(illust, pages, following, epoch);
    }
}

impl App {
    pub(crate) fn install_detail_with_follow(
        &mut self,
        illust: Illust,
        pages: Vec<PageMeta>,
        following: bool,
        epoch: u64,
    ) {
        if self.detail_epoch != epoch {
            return;
        }

        {
            let ui = self.ui();
            ui.set_detail_title(illust.title.clone().into());
            ui.set_detail_author(illust.author.name.clone().into());
            ui.set_detail_author_id(illust.author.id.to_string().into());
            ui.set_detail_description(illust.description.clone().into());
            ui.set_detail_posted(illust.create_date.clone().unwrap_or_default().into());
            ui.set_detail_kind(illust.kind.code());
            ui.set_detail_page_count(illust.page_count as i32);
            ui.set_detail_views(illust.stats.views as i32);
            ui.set_detail_bookmarks(illust.bookmark.count as i32);
            ui.set_detail_likes(illust.stats.likes as i32);
            ui.set_detail_bookmarked(illust.bookmark.bookmarked);
            ui.set_detail_following(following);
            ui.set_detail_ugoira_hint(
                if illust.kind == pixiv_types::domain::IllustKind::Ugoira {
                    "可导出为 GIF 保存在下载目录；播放尚未内置，请在浏览器中查看原始动图。"
                } else {
                    ""
                }
                .into(),
            );
            let tags: Vec<slint::SharedString> =
                illust.tags.iter().map(|t| t.display().into()).collect();
            ui.set_detail_tags(ModelRc::from(Rc::new(VecModel::from(tags))));

            // The author's face, from the same map the cards use. A work payload
            // usually carries the avatar URL; when it does not, the id lookup the
            // cards already do is reused rather than asking for a second endpoint.
            let avatar_url = illust
                .author
                .avatar
                .clone()
                .or_else(|| self.author_avatars.get(&illust.author.id.get()).cloned());
            let avatar = avatar_url
                .as_deref()
                .and_then(|url| self.avatars.get(url).cloned());
            match &avatar {
                Some(image) => {
                    ui.set_detail_author_avatar(image.clone());
                    ui.set_detail_author_avatar_state(BAND_OK);
                }
                None => {
                    ui.set_detail_author_avatar(slint::Image::default());
                    ui.set_detail_author_avatar_state(if avatar_url.is_some() {
                        BAND_LOADING
                    } else {
                        BAND_EMPTY
                    });
                }
            }
            self.detail_author_avatar_url = avatar_url;
        }

        // Demo works must never enter the on-disk history: their cover URLs are
        // `demo://` pseudo-URLs that cannot be fetched again, so a later real-mode
        // History page would show cards that can never load. The two modes stay
        // separate, which is the whole point of the demo badge.
        if self.mode == Mode::Real {
            // Queued, not written here: `record_view` takes the store mutex and
            // scans MAX(seq) on the thread that paints (plan §2.3 R16).
            self.record_view(HistoryEntry {
                illust_id: illust.id.to_string(),
                title: illust.title.clone(),
                author: illust.author.name.clone(),
                thumb_url: illust.urls.grid().to_string(),
                viewed_at: now_secs(),
            });
        }

        // Page skeletons, sized from the real page metadata when we have it.
        let count = (illust.page_count.max(1) as usize).max(pages.len());
        let skeletons: Vec<ViewerPage> = (0..count)
            .map(|index| {
                let meta = pages.get(index);
                ViewerPage {
                    source: slint::Image::default(),
                    thumb: slint::Image::default(),
                    width: meta.map_or(illust.width as i32, |p| p.width as i32),
                    height: meta.map_or(illust.height as i32, |p| p.height as i32),
                    loaded: false,
                    failed: false,
                }
            })
            .collect();
        let models = Rc::new(VecModel::from(skeletons));
        self.ui().set_detail_pages(ModelRc::from(models.clone()));
        self.ui().set_detail_state(0);

        let urls: Vec<String> = if pages.is_empty() {
            vec![illust.urls.largest().to_string()]
        } else {
            pages
                .iter()
                .map(|page| page.urls.regular.to_string())
                .collect()
        };

        let mode = self.mode;
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let seed = illust.id;
        let target = models.clone();

        rt.spawn(async move {
            let mut stream = futures::stream::iter(urls.into_iter().enumerate())
                .map(move |(index, url)| {
                    let cache = cache.clone();
                    async move {
                        let raw = if mode == Mode::Demo {
                            demo_raw(
                                Id(seed.get().wrapping_add(index as u64)),
                                900,
                                1200,
                                index as u64,
                            )
                            .await
                        } else {
                            fetch_sized_viewer(&cache, &url).await
                        };
                        (index, raw)
                    }
                })
                .buffer_unordered(3);

            while let Some((index, raw)) = stream.next().await {
                on_ui(move |shared| {
                    let Ok(app) = shared.try_borrow() else {
                        return;
                    };
                    let Some(model) = app.detail_models.clone() else {
                        return;
                    };
                    if app.detail_epoch != epoch {
                        return;
                    }
                    let Some(mut page) = model.row_data(index) else {
                        return;
                    };
                    match raw {
                        Some(raw) => {
                            let image = raw.to_slint();
                            page.source = image.clone();
                            page.thumb = image;
                            page.loaded = true;
                        }
                        None => page.failed = true,
                    }
                    model.set_row_data(index, page);
                });
            }
        });

        self.detail = Some(illust);
        self.detail_pages = pages;
        self.detail_models = Some(target);

        // Only now, with this page owning the work: the bounded id lookup reads
        // `self.detail`, so asking before the move resolves the *previous* page's
        // author, which is what the first attempt did.
        match self.detail_author_avatar_url.clone() {
            Some(url) => self.spawn_avatars(vec![url]),
            None => self.backfill_author_avatars(),
        }
        self.refresh_chrome();
    }
}

impl App {
    /// Re-fetch the image of the page the viewer is showing.
    ///
    /// The failed state has had a 「重新载入」 button since the viewer was written,
    /// but it was wired to `request-toggle-zoom`: pressing it flipped between fit
    /// and 1:1 and never requested the image again. One page is retried, not the
    /// whole work - a 40-page illustration should not refetch 40 images to recover
    /// from one timeout.
    pub(crate) fn reload_detail_page(&self) {
        let Some(models) = self.detail_models.clone() else {
            return;
        };
        let Some(illust) = self.detail.clone() else {
            return;
        };
        let index = self.ui().get_detail_page_index().max(0) as usize;
        let Some(mut page) = models.row_data(index) else {
            return;
        };

        let Some(url) = retry_url(&self.detail_pages, illust.urls.largest(), index) else {
            // A work with no image at all - a novel, a deleted page, a page list
            // that came back empty - has nothing to retry. Saying so is the whole
            // difference between a button that is broken and one that has no work
            // to do.
            self.notify(
                2,
                "这件作品没有可重新载入的图片，可以点工具栏的「在浏览器中打开」",
            );
            return;
        };

        // Back to "loading" first: leaving `failed` set would keep the error screen
        // up while the retry is in flight, which is exactly the lie the old button
        // told.
        page.loaded = false;
        page.failed = false;
        models.set_row_data(index, page);

        let mode = self.mode;
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let epoch = self.detail_epoch;
        let seed = illust.id;
        rt.spawn(async move {
            let raw = if mode == Mode::Demo {
                demo_raw(
                    Id(seed.get().wrapping_add(index as u64)),
                    900,
                    1200,
                    index as u64,
                )
                .await
            } else {
                fetch_sized_viewer(&cache, &url).await
            };
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                if app.detail_epoch != epoch {
                    return;
                }
                let Some(model) = app.detail_models.clone() else {
                    return;
                };
                let Some(mut page) = model.row_data(index) else {
                    return;
                };
                match raw {
                    Some(raw) => {
                        let image = raw.to_slint();
                        page.source = image.clone();
                        page.thumb = image;
                        page.loaded = true;
                        page.failed = false;
                    }
                    None => {
                        page.loaded = false;
                        page.failed = true;
                    }
                }
                model.set_row_data(index, page);
            });
        });
    }
}

impl App {
    pub(crate) fn toggle_bookmark(&mut self, id: &str) {
        let Ok(raw) = id.parse::<u64>() else {
            return;
        };
        let illust_id = Id(raw);

        match self.mode {
            Mode::Demo => {
                if let Some(item) = self.items.iter_mut().find(|i| i.id == illust_id) {
                    let next = !item.bookmark.bookmarked;
                    item.apply_bookmark(next, item.bookmark.restrict);
                }
                let bookmarked = self.bookmarked_state(illust_id);
                let ui = self.ui();
                if self.page == Destination::Detail {
                    ui.set_detail_bookmarked(bookmarked);
                }
                self.notify(2, "演示模式：收藏状态只在本机切换，不会提交到 Pixiv");
                self.resync_bookmarks();
            }
            Mode::Real => {
                if !self.session.is_authenticated() {
                    self.notify(2, "需要登录后才能收藏，请到设置页登录");
                    return;
                }
                let Some(client) = self.client.clone() else {
                    return;
                };
                // Decide the direction from what the server last told us, and
                // carry the current visibility so a private bookmark stays
                // private.
                let current = self
                    .detail
                    .as_ref()
                    .filter(|d| d.id == illust_id)
                    .map(|d| d.bookmark)
                    .unwrap_or_default();
                let next = !current.bookmarked;
                let restrict = if current.bookmarked {
                    current.restrict
                } else {
                    BookmarkRestrict::Public
                };
                let rt = self.rt.clone();
                rt.spawn(async move {
                    let result = client.set_bookmark(illust_id, next, restrict).await;
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        match result {
                            Ok(state) => {
                                // Apply the state the server reported, not the
                                // one we asked for: if Pixiv refused, this is the
                                // old value and the UI stays truthful.
                                let now_bookmarked = state.bookmarked;
                                app.apply_bookmark_state(illust_id, state);
                                app.notify(
                                    1,
                                    if now_bookmarked {
                                        "已加入收藏"
                                    } else {
                                        "已取消收藏"
                                    },
                                );
                            }
                            Err(error) => {
                                // Leave the UI on the previous state and say why.
                                app.notify(3, format!("收藏失败：{error}"));
                            }
                        }
                    });
                });
            }
        }
    }
}

impl App {
    pub(crate) fn bookmarked_state(&self, id: Id) -> bool {
        self.items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.bookmark.bookmarked)
            .or_else(|| {
                self.detail
                    .as_ref()
                    .filter(|d| d.id == id)
                    .map(|d| d.bookmark.bookmarked)
            })
            .unwrap_or(false)
    }
}

impl App {
    /// Adopt the server's bookmark verdict everywhere the illustration appears.
    ///
    /// The feed card, the detail model and the metadata list each hold their own
    /// copy, and they used to drift: toggling on the detail page left the card in
    /// the grid showing the opposite state until the page was reloaded.
    pub(crate) fn apply_bookmark_state(&mut self, id: Id, state: pixiv_types::BookmarkState) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.apply_bookmark(state.bookmarked, state.restrict);
        }
        if let Some(detail) = self.detail.as_mut().filter(|d| d.id == id) {
            detail.apply_bookmark(state.bookmarked, state.restrict);
            let count = detail.bookmark.count as i32;
            let ui = self.ui();
            ui.set_detail_bookmarks(count);
            ui.set_detail_bookmarked(state.bookmarked);
        }
        self.resync_bookmarks();
    }
}

impl App {
    /// Push the in-memory bookmark state back into the visible cards.
    pub(crate) fn resync_bookmarks(&mut self) {
        let map: HashMap<String, bool> = self
            .items
            .iter()
            .map(|i| (i.id.to_string(), i.bookmark.bookmarked))
            .collect();
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                if let Some(flag) = map.get(item.id.as_str())
                    && item.bookmarked != *flag
                {
                    item.bookmarked = *flag;
                    column.set_row_data(row, item);
                }
            }
        }
    }
}

impl App {
    /// Toggle the follow state for the current author.
    ///
    /// The button's own label is not evidence of anything: it is a cached render.
    /// The server is asked first, then told to change its mind, then asked again
    /// to confirm — because `POST /ajax/user/follow` never existed, and the
    /// version that assumed success reported a follow that had not happened.
    pub(crate) fn toggle_follow(&mut self) {
        let Some(illust) = self.detail.clone() else {
            return;
        };
        if !self.session.is_authenticated() {
            self.notify(2, "需要登录后才能关注画师");
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let artist = illust.author.id;
        self.ui().set_detail_busy(true);
        let rt = self.rt.clone();
        rt.spawn(async move {
            let outcome = async {
                let current = client.follow_state(artist).await?;
                let target = !current;
                client.follow(artist, target).await?;
                let confirmed = client.follow_state(artist).await?;
                Ok::<_, PixivError>((target, confirmed))
            }
            .await;
            on_ui(move |shared| {
                let Ok(app) = shared.try_borrow() else {
                    return;
                };
                app.ui().set_detail_busy(false);
                match outcome {
                    Ok((target, confirmed)) => {
                        app.ui().set_detail_following(confirmed);
                        if confirmed == target {
                            app.notify(
                                1,
                                if confirmed {
                                    "已关注画师"
                                } else {
                                    "已取消关注"
                                },
                            );
                        } else {
                            // The server disagreed with the request: say so rather
                            // than claim the click worked.
                            app.notify(
                                3,
                                if confirmed {
                                    "关注状态未改变：服务器仍然显示已关注"
                                } else {
                                    "关注状态未改变：服务器仍然显示未关注"
                                },
                            );
                        }
                    }
                    Err(error) => app.notify(3, format!("关注失败：{error}")),
                }
            });
        });
    }
}
