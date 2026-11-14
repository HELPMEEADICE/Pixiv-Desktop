use super::*;

mod avatars;
mod discovery;
mod home;
mod search;

pub(crate) use self::{discovery::*, search::*};

impl App {
    // ------------------------------------------------------------- feed

    pub(crate) fn render_feed(&mut self, items: Vec<Illust>, append: bool) {
        let mut all = if append {
            std::mem::take(&mut self.items)
        } else {
            Vec::new()
        };
        if items.is_empty() && all.is_empty() {
            let ui = self.ui();
            ui.set_grid_state(2);
            // The headline names what came back empty; the message says what to do
            // about it. One generic sentence for every page read as filler.
            let (headline, glyph, hint) = match self.page {
                Destination::Search => {
                    ("没有匹配的作品", "search", "换个关键词，或用标签再试一次。")
                }
                Destination::Bookmarks => (
                    "收藏夹是空的",
                    "star",
                    "在作品详情页点击「收藏」后，会出现在这里。",
                ),
                Destination::History => (
                    "还没有浏览记录",
                    "history",
                    "本机看过的作品会记录在这里，仅保存在本地。",
                ),
                _ => ("这里还没有内容", "", "试试刷新，或换一个标签。"),
            };
            ui.set_grid_state_title(headline.into());
            ui.set_grid_state_glyph(glyph.into());
            ui.set_grid_message(hint.into());
            ui.set_grid_action_label("".into());
            ui.set_grid_count_label("".into());
            ui.set_grid_columns(ModelRc::default());
            self.columns.clear();
            self.index.clear();
            self.reached_end = true;
            return;
        }
        let already = self.items.len();
        all.extend(items);
        self.items = all;
        if !append {
            // A feed replacement is the one moment the set of live works is known
            // exactly, so it is also the only collection point that needs no timer
            // and no second opinion about what is on screen.
            let ids = self.items.iter().map(|illust| illust.id.to_string());
            prune_covers(&mut self.covers, ids, COVERS_KEEP);
        }
        // A new page extends the columns it finds. Rebuilding them re-ran the
        // balancer over the whole feed, so every card above silently moved to a
        // different column - the opposite of what `layout::distribute` is documented
        // to preserve, and visible as a jump while scrolling.
        if append
            && already > 0
            && self.columns.len() == self.geometry.columns
            && self.column_heights.len() == self.columns.len()
        {
            self.append_columns(already);
        } else {
            self.rebuild_columns();
        }
    }
}

impl App {
    /// Recompute the column buckets from `self.items` for the current geometry.
    ///
    /// This is the expensive path (it rebuilds every row model), so it is only
    /// called when the *column count* changes or when the feed itself changes.
    /// A pure width change keeps the buckets and only updates `card-width`, which
    /// is one property assignment — that is what stops a window drag from
    /// rebuilding the grid on every frame.
    pub(crate) fn rebuild_columns(&mut self) {
        let geometry = self.geometry;
        let heights: Vec<f32> = self
            .items
            .iter()
            .map(|illust| {
                layout::card_height(
                    geometry.card_width,
                    layout::clamped_aspect(illust.width, illust.height),
                )
            })
            .collect();
        let assignment = layout::distribute(&heights, geometry.columns);
        let buckets = layout::bucket(&assignment, geometry.columns);
        // Sum what each column actually received, so an appended page can be placed
        // without re-reading the models or recomputing the feed.
        let heights_by_column: Vec<f32> = buckets
            .iter()
            .map(|rows| rows.iter().map(|index| heights[*index]).sum())
            .collect();

        let mut models = Vec::with_capacity(buckets.len());
        let mut index = HashMap::new();
        for (column, rows) in buckets.iter().enumerate() {
            let rows: Vec<IllustItem> = rows
                .iter()
                .enumerate()
                .map(|(row, item_index)| {
                    let illust = &self.items[*item_index];
                    let mut item = illust_to_item(illust, self.rank_of(illust));
                    let key = illust.id.to_string();
                    if let Some(image) = self.covers.get(&key) {
                        item.cover = image.clone();
                        item.state = 0;
                    }
                    index.insert(key, (column, row));
                    item
                })
                .collect();
            models.push(Rc::new(VecModel::from(rows)));
        }

        let outer: Vec<ModelRc<IllustItem>> =
            models.iter().map(|m| ModelRc::from(m.clone())).collect();

        let ui = self.ui();
        ui.set_grid_columns(ModelRc::from(Rc::new(VecModel::from(outer))));
        ui.set_grid_card_width(geometry.card_width);
        ui.set_grid_state(0);
        ui.set_grid_action_label("".into());
        ui.set_grid_count_label(format!("{} 件作品", self.items.len()).into());
        ui.set_grid_message("".into());
        ui.set_status_text(self.status_line().into());

        self.columns = models;
        self.column_heights = heights_by_column;
        self.index = index;
        self.spawn_covers();
        self.backfill_author_avatars();
    }
}

impl App {
    /// Place the items that arrived after row `from` into the existing columns.
    ///
    /// Uses the same greedy rule as [`layout::distribute`] - shortest column, ties
    /// to the left - applied one item at a time against the accumulated heights.
    /// Nothing above the new page is touched, so the feed does not reshuffle while
    /// the user is reading it.
    pub(crate) fn append_columns(&mut self, from: usize) {
        let geometry = self.geometry;
        for index in from..self.items.len() {
            let illust = &self.items[index];
            let height = layout::card_height(
                geometry.card_width,
                layout::clamped_aspect(illust.width, illust.height),
            );
            let target = shortest_column(&self.column_heights);
            let mut item = illust_to_item(illust, self.rank_of(illust));
            let key = illust.id.to_string();
            if let Some(image) = self.covers.get(&key) {
                item.cover = image.clone();
                item.state = 0;
            }
            let Some(model) = self.columns.get(target) else {
                continue;
            };
            let row = model.row_count();
            model.push(item);
            self.index.insert(key, (target, row));
            self.column_heights[target] += height;
        }

        let ui = self.ui();
        ui.set_grid_card_width(geometry.card_width);
        ui.set_grid_count_label(format!("{} 件作品", self.items.len()).into());
        ui.set_status_text(self.status_line().into());
        self.spawn_covers();
        self.backfill_author_avatars();
    }
}

impl App {
    pub(crate) fn rank_of(&self, illust: &Illust) -> i32 {
        if self.page != Destination::Ranking {
            return 0;
        }
        self.items
            .iter()
            .position(|candidate| candidate.id == illust.id)
            .map(|index| index as i32 + 1)
            .unwrap_or(0)
    }
}

impl App {
    /// Start cover loads for anything not already cached or in flight.
    pub(crate) fn spawn_covers(&mut self) {
        let urls: Vec<(String, String)> = self
            .items
            .iter()
            .map(|illust| (illust.id.to_string(), illust.urls.grid().to_string()))
            .collect();
        let cached = &self.covers;
        let plan = self.loader.plan(urls, |id| cached.contains_key(id));
        if plan.is_empty() {
            return;
        }

        let epoch = self.epoch;
        let demo = self.mode == Mode::Demo;
        let cache = self.cache.clone();
        let rt = self.rt.clone();
        let scale = self.ui().window().scale_factor();
        let target = crate::images::target_width(self.geometry.card_width, scale);

        rt.spawn(async move {
            let cache_for = cache.clone();
            let mut stream = futures::stream::iter(plan)
                .map(move |request| {
                    let cache = cache_for.clone();
                    async move {
                        let raw = if demo {
                            demo_raw(
                                Id(request.id.parse::<u64>().unwrap_or(0)),
                                target,
                                target,
                                0,
                            )
                            .await
                        } else {
                            fetch_sized(&cache, &request.url, target).await
                        };
                        (request.id, raw)
                    }
                })
                .buffer_unordered(COVER_CONCURRENCY);

            while let Some((id, raw)) = stream.next().await {
                let ok = raw.is_some();
                on_ui(move |shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    app.loader.complete(&id, ok);
                    // A result for a feed the user already left must not touch
                    // the current grid.
                    if app.epoch != epoch {
                        return;
                    }
                    match raw {
                        Some(raw) => app.apply_cover(&id, &raw),
                        None => app.mark_cover_failed(&id),
                    }
                });
            }
        });
    }
}

impl App {
    pub(crate) fn apply_cover(&mut self, id: &str, raw: &RawImage) {
        let image = raw.to_slint();
        self.covers.insert(id.to_string(), image.clone());
        let Some((column, row)) = self.index.get(id).copied() else {
            return;
        };
        let Some(model) = self.columns.get(column) else {
            return;
        };
        if let Some(mut item) = model.row_data(row) {
            item.cover = image;
            item.state = 0;
            model.set_row_data(row, item);
        }
    }
}

impl App {
    pub(crate) fn mark_cover_failed(&mut self, id: &str) {
        let Some((column, row)) = self.index.get(id).copied() else {
            return;
        };
        let Some(model) = self.columns.get(column) else {
            return;
        };
        if let Some(mut item) = model.row_data(row) {
            item.state = 2;
            model.set_row_data(row, item);
        }
    }
}

impl App {
    /// Slint reported a new window width. This is the only place the shell's
    /// narrow/compact decision is made, and it is event-driven: the previous
    /// implementation polled `Window::size()` every 250ms *and* reported the width
    /// from a Slint callback, so the two could disagree for a quarter of a second
    /// on every resize.
    pub(crate) fn on_window_width(&mut self, width: f32) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        const NARROW_BREAKPOINT: f32 = 1000.0;

        let narrow = width < NARROW_BREAKPOINT;
        let collapsed = self.ui().get_rail_collapsed();
        // Read the rail widths from the theme rather than restating them here: they
        // were duplicated as 240/52 while `theme.slint` said 232/52, so the content
        // box this function reported was 8px wider than the one the layout used.
        let ui = self.ui();
        let rail = {
            let theme = ui.global::<Theme>();
            if narrow || collapsed {
                theme.get_rail_width_compact()
            } else {
                theme.get_rail_width()
            }
        };
        let content = (width - rail).max(0.0);
        ui.set_narrow(narrow);
        ui.set_content_width(content);
    }
}

impl App {
    /// Slint reported the content width it actually resolved. Recompute geometry.
    pub(crate) fn on_measured(&mut self, width: f32) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        if (width - self.measured_width).abs() < 0.5 {
            return;
        }
        self.measured_width = width;
        // The profile header is the one page that changes *shape* rather than just
        // density, so it needs the same measured number the grid is built from.
        self.ui().set_profile_wide(width >= PROFILE_WIDE_MIN);
        let geometry = GridGeometry::for_width(width);
        let columns_changed = geometry.columns != self.geometry.columns;
        let width_changed = (geometry.card_width - self.geometry.card_width).abs() >= 0.5;
        self.geometry = geometry;
        if !width_changed && !columns_changed {
            return;
        }
        if columns_changed && !self.items.is_empty() {
            self.rebuild_columns();
        } else {
            self.ui().set_grid_card_width(geometry.card_width);
        }
    }
}

/// The four independent bands of the home page.
pub(crate) struct HomeBands {
    pub(crate) works: Vec<Illust>,
    pub(crate) latest: Vec<Illust>,
    pub(crate) latest_title: String,
    pub(crate) tags: Vec<RecommendTag>,
    pub(crate) artists: Vec<RecommendUser>,
    pub(crate) works_state: i32,
    pub(crate) latest_state: i32,
    pub(crate) tags_state: i32,
    pub(crate) artists_state: i32,
    pub(crate) works_message: String,
    pub(crate) latest_message: String,
    pub(crate) tags_message: String,
    pub(crate) artists_message: String,
    /// Extra "load more" batches appended to the for-you grid.
    pub(crate) batches: u32,
    pub(crate) empty_batches: u32,
    pub(crate) exhausted: bool,
}

impl HomeBands {
    pub(crate) fn loading() -> Self {
        Self {
            works: Vec::new(),
            latest: Vec::new(),
            latest_title: String::new(),
            tags: Vec::new(),
            artists: Vec::new(),
            works_state: BAND_LOADING,
            latest_state: BAND_LOADING,
            tags_state: BAND_LOADING,
            artists_state: BAND_LOADING,
            works_message: String::new(),
            latest_message: String::new(),
            tags_message: String::new(),
            artists_message: String::new(),
            batches: 0,
            empty_batches: 0,
            exhausted: false,
        }
    }
}

impl App {
    /// One card, with whatever avatar is already decoded.
    pub(crate) fn illust_item_avatar(&self, illust: &Illust, rank: i32) -> IllustItem {
        let mut item = illust_to_item(illust, rank);
        if let Some(url) = illust.author.avatar.as_ref()
            && let Some(image) = self.avatars.get(url)
        {
            item.author_avatar = image.clone();
            item.author_avatar_state = BAND_OK;
        }
        item
    }
}

impl App {
    /// Every image one recommended-user card needs: their avatar and the cover
    /// of the work used as a preview. Without this the artist row is drawn from
    /// placeholders, which is exactly what "no avatar" looked like.
    pub(crate) fn user_image_urls(users: &[RecommendUser]) -> Vec<String> {
        let mut urls = Vec::new();
        for user in users {
            urls.extend(user.avatar.clone());
            if let Some(preview) = user.previews.first() {
                let cover = preview.urls.grid();
                if !cover.is_empty() {
                    urls.push(cover.to_string());
                }
            }
        }
        urls
    }
}

impl App {
    pub(crate) fn user_item(&self, user: &RecommendUser) -> RecommendUserItem {
        let avatar_url = user.avatar.clone().unwrap_or_default();
        let preview = user
            .previews
            .first()
            .and_then(|illust| self.avatars.get(illust.urls.grid()))
            .cloned()
            .unwrap_or_default();
        RecommendUserItem {
            id: user.id.to_string().into(),
            name: user.name.clone().into(),
            account: user.account.clone().into(),
            comment: user.comment.clone().into(),
            avatar: self.avatars.get(&avatar_url).cloned().unwrap_or_default(),
            avatar_url: avatar_url.clone().into(),
            preview,
            followed: user.is_followed,
            premium: user.is_premium,
            avatar_state: if avatar_url.is_empty() {
                BAND_EMPTY
            } else if self.avatars.contains_key(&avatar_url) {
                BAND_OK
            } else {
                BAND_LOADING
            },
        }
    }
}
