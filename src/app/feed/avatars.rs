use super::*;

impl App {
    // ------------------------------------------------------------- avatars

    /// Take one URL to a decoded image, so a card, a header and the top bar can
    /// share one download.
    pub(crate) fn start_avatar(&mut self, url: &str) -> bool {
        if url.trim().is_empty()
            || self.avatars.contains_key(url)
            || self.avatar_loading.contains(url)
        {
            return false;
        }
        self.avatar_loading.insert(url.to_string());
        true
    }
}

impl App {
    pub(crate) fn spawn_avatars(&mut self, urls: Vec<String>) {
        let mut plan: Vec<String> = Vec::new();
        for url in urls {
            if self.start_avatar(&url) {
                plan.push(url);
            }
        }
        if plan.is_empty() {
            return;
        }
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let demo = self.mode == Mode::Demo;
        let scale = self.ui().window().scale_factor().max(1.0);
        // A 170px source is the largest Pixiv sends for an avatar; decoding at
        // the size actually drawn keeps 40 avatars off the memory budget.
        let target = crate::images::target_width(96.0, scale);
        rt.spawn(async move {
            let mut stream = futures::stream::iter(plan)
                .map(move |url| {
                    let cache = cache.clone();
                    async move {
                        let raw = if demo {
                            None
                        } else {
                            fetch_sized(&cache, &url, target).await
                        };
                        (url, raw)
                    }
                })
                .buffer_unordered(4);
            while let Some((url, raw)) = stream.next().await {
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.avatar_loading.remove(&url);
                    let Some(raw) = raw else {
                        return;
                    };
                    let image = raw.to_slint();
                    app.avatars.insert(url.clone(), image);
                    // Rows on screen already hold their own copy of the image, so
                    // dropping the map never blanks a visible face: the next render
                    // re-requests it and gets it from the disk/memory tier.
                    if app.avatars.len() > AVATARS_KEEP {
                        app.avatars.clear();
                    }
                    app.repaint_avatars(&url);
                });
            }
        });
    }
}

impl App {
    /// Patch every surface that shows the avatar that just arrived.
    pub(crate) fn repaint_avatars(&mut self, url: &str) {
        if self.account_avatar.as_deref() == Some(url)
            && let Some(image) = self.avatars.get(url).cloned()
        {
            let ui = self.ui();
            ui.set_account_avatar(image);
            ui.set_account_avatar_state(BAND_OK);
        }

        if self
            .home
            .artists
            .iter()
            .any(|user| user.avatar.as_deref() == Some(url))
        {
            self.publish_home();
        }
        if self
            .discovery
            .users
            .iter()
            .any(|user| user.avatar.as_deref() == Some(url))
        {
            self.publish_discovery_users();
        }
        if self
            .profile
            .as_ref()
            .and_then(|state| state.profile.avatar.clone())
            .as_deref()
            == Some(url)
        {
            self.publish_profile();
        }

        if self.detail_author_avatar_url.as_deref() == Some(url)
            && let Some(image) = self.avatars.get(url).cloned()
        {
            let ui = self.ui();
            ui.set_detail_author_avatar(image);
            ui.set_detail_author_avatar_state(BAND_OK);
        }

        // Cards: match on the URL the row was built with, so only the rows that
        // wanted this face are patched.
        let Some(image) = self.avatars.get(url).cloned() else {
            return;
        };
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                if item.author_avatar_url.as_str() != url {
                    continue;
                }
                item.author_avatar = image.clone();
                item.author_avatar_state = BAND_OK;
                column.set_row_data(row, item);
            }
        }
    }
}

impl App {
    /// Resolve the faces of authors whose payload carried no avatar URL.
    ///
    /// Ranking and search rows do name a profile image, but a page assembled
    /// from several sources can contain authors that did not. Rather than one
    /// profile request per card (an N+1 storm), at most four distinct unknown
    /// authors are resolved per render, the result is cached by user id, and an
    /// id that is already in flight is never requested twice.
    pub(crate) fn backfill_author_avatars(&mut self) {
        if self.mode != Mode::Real {
            return;
        }
        let mut wanted: Vec<u64> = Vec::new();
        // The open detail page is the one the user is looking at, so its author
        // gets first claim on the four lookups this render is allowed.
        if let Some(detail) = &self.detail
            && detail.author.id.get() != 0
            && detail.author.avatar.is_none()
            && !self.author_avatars.contains_key(&detail.author.id.get())
            && !self.author_avatar_lookup.contains(&detail.author.id.get())
        {
            wanted.push(detail.author.id.get());
        }
        for illust in &self.items {
            let author = illust.author.id.get();
            if author == 0 || illust.author.avatar.is_some() {
                continue;
            }
            if self.author_avatars.contains_key(&author)
                || self.author_avatar_lookup.contains(&author)
                || wanted.contains(&author)
            {
                continue;
            }
            wanted.push(author);
            if wanted.len() == 4 {
                break;
            }
        }
        if wanted.is_empty() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        for author in &wanted {
            self.author_avatar_lookup.insert(*author);
        }
        let rt = self.rt.clone();
        rt.spawn(async move {
            for author in wanted {
                let found = client
                    .user_profile(Id(author))
                    .await
                    .ok()
                    .and_then(|profile| profile.avatar);
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.author_avatar_lookup.remove(&author);
                    let Some(url) = found else {
                        return;
                    };
                    app.author_avatars.insert(author, url.clone());
                    app.apply_author_avatar(author, &url);
                    app.adopt_detail_author_avatar(author, &url);
                    app.spawn_avatars(vec![url]);
                });
            }
        });
    }
}

impl App {
    /// Point every card of one author at the avatar that was just resolved.
    /// Point the open detail panel at an avatar that was just resolved by id.
    pub(crate) fn adopt_detail_author_avatar(&mut self, author: u64, url: &str) {
        let Some(detail) = &self.detail else {
            return;
        };
        if detail.author.id.get() != author {
            return;
        }
        self.detail_author_avatar_url = Some(url.to_string());
        let ui = self.ui();
        match self.avatars.get(url).cloned() {
            Some(image) => {
                ui.set_detail_author_avatar(image);
                ui.set_detail_author_avatar_state(BAND_OK);
            }
            None => ui.set_detail_author_avatar_state(BAND_LOADING),
        }
    }
}

impl App {
    pub(crate) fn apply_author_avatar(&mut self, author: u64, url: &str) {
        let key = author.to_string();
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                if item.author_id.as_str() != key || !item.author_avatar_url.is_empty() {
                    continue;
                }
                item.author_avatar_url = url.into();
                item.author_avatar_state = BAND_LOADING;
                column.set_row_data(row, item);
            }
        }
    }
}
