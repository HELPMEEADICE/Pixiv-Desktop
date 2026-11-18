use super::*;

use super::ugoira::{demo_film, film_width_for};
use pixiv_ugoira::UgoiraFilm;

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
            app.leave_page(current, target);
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
            // A film belongs to the work that opened it. Opening another work
            // stops the clock and releases every frame of the old one before the
            // new page has anything to show (F1).
            app.release_playback();
            (app.mode, epoch)
        };

        match mode {
            Mode::Demo => {
                let app = shared.borrow();
                let found = app.demo.detail(illust_id);
                let pages = app.demo.pages(illust_id);
                drop(app);
                let Some(mut illust) = found else {
                    App::fail_detail(shared, epoch, "找不到该作品，或它对该账号不可见。");
                    return;
                };
                // F2: a capture cannot reach a work Pixiv marks sensitive offline —
                // every demo row carries `is_masked: false` — so the state has a name
                // the capture table can ask for (G24). Nothing else reads it.
                match crate::app::feed::fixture::current() {
                    crate::app::feed::fixture::Fixture::Sensitive => {
                        illust.is_masked = true;
                        illust.sl = 2;
                    }
                    crate::app::feed::fixture::Fixture::Ai => illust.ai_type = 1,
                    _ => {}
                }
                let mut app = shared.borrow_mut();
                app.install_detail_with_follow(
                    DetailPayload {
                        illust,
                        pages,
                        pages_failed: false,
                        // The demo catalogue has no follow state of its own; it is
                        // the demo, and the page says so.
                        following: FollowState::NotFollowing,
                    },
                    epoch,
                );
            }
            Mode::Real => {
                let app = shared.borrow();
                let client = app.client.clone();
                let rt = app.tasks.clone();
                drop(app);
                let Some(client) = client else {
                    App::fail_detail(shared, epoch, "没有可用的网络客户端。");
                    return;
                };
                rt.spawn(async move {
                    // Detail, pages and follow-state in one round trip batch: the
                    // page used to show a single page for every real artwork
                    // because the page list was never fetched.
                    let illust = match client.illust_detail(illust_id).await {
                        Ok(illust) => illust,
                        Err(error) => {
                            on_ui(move |shared| {
                                App::fail_detail(shared, epoch, &describe_session_error(&error));
                            });
                            return;
                        }
                    };
                    // T1: the two side reads keep their Results. A failed page list
                    // used to be `Vec::new()` and a failed profile read `false`, so a
                    // 30-page work rendered as one page and an unread artist was
                    // reported as 「未关注」.
                    let pages = match client.illust_pages(illust_id).await {
                        Ok(pages) => Ok(pages),
                        Err(error) => {
                            tracing::warn!(
                                illust = illust_id.get(),
                                %error,
                                "page list failed; the payload's page count stands"
                            );
                            Err(error)
                        }
                    };
                    let follow = match client.user_profile(illust.author.id).await {
                        Ok(profile) => Ok(profile.is_followed),
                        Err(error) => {
                            tracing::warn!(
                                artist = illust.author.id.get(),
                                %error,
                                "follow state could not be read"
                            );
                            Err(error)
                        }
                    };
                    let payload = read_detail(illust, pages, follow);
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.detail_epoch != epoch {
                            return;
                        }
                        app.install_detail_with_follow(payload, epoch);
                    });
                });
            }
        }
    }
}

impl App {
    /// Say that the illustration page could not be read.
    ///
    /// The epoch is compared **first**, exactly like the success path
    /// (`open_detail_route` installs a payload only when `detail_epoch` still agrees):
    /// a slow failed read of A used to write `detail-state = 3` over the
    /// already-loaded B, so B's viewer showed A's error, and the route was not
    /// restored either. `reply_lands` is the one spelling of that comparison, shared
    /// with the follow reply (B4), the user page (B2) and the re-tag (B5).
    pub(crate) fn fail_detail(shared: &Shared, epoch: u64, message: &str) {
        let app = shared.borrow();
        if !reply_lands(
            ReplySurface::Detail(app.detail_epoch),
            ReplySurface::Detail(epoch),
        ) {
            return;
        }
        let ui = app.ui();
        ui.set_detail_state(3);
        ui.set_detail_message(message.into());
    }
}

/// What the three detail requests answered, failures included.
pub(crate) struct DetailPayload {
    pub illust: Illust,
    /// Page metadata, empty when the page-list read failed.
    pub pages: Vec<PageMeta>,
    /// True when the page list could not be read: the viewer still draws
    /// `illust.page_count` cells and the page says where the pages went (T1).
    pub pages_failed: bool,
    pub following: FollowState,
}

/// What the page knows about the author's follow state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FollowState {
    Following,
    NotFollowing,
    /// The profile read failed, so the page must not claim either state (T1).
    Unknown,
}

impl FollowState {
    /// The value `detail-following` can carry; `None` when the page does not know.
    ///
    /// `detail-following` is a bool and `ui/` is not this track's to change, so an
    /// unknown state is published through `detail-message` plus one shell notice
    /// rather than by drawing 「未关注」 (see the track report: a tri-state property
    /// is the honest home for it).
    pub(crate) fn known(self) -> Option<bool> {
        match self {
            Self::Following => Some(true),
            Self::NotFollowing => Some(false),
            Self::Unknown => None,
        }
    }
}

/// Assemble the detail payload, keeping both side reads' answers.
///
/// The page list and the profile read used to be collapsed into `Vec::new()` and
/// `false` (T1), which is how a 30-page work came back looking one page long and
/// every unread artist looked unfollowed. Both Results are kept: a failure is a
/// failure, and the page has a sentence for each.
pub(crate) fn read_detail(
    illust: Illust,
    pages: Result<Vec<PageMeta>, PixivError>,
    follow: Result<bool, PixivError>,
) -> DetailPayload {
    let (pages, pages_failed) = match pages {
        Ok(pages) => (pages, false),
        Err(_) => (Vec::new(), true),
    };
    let following = match follow {
        Ok(true) => FollowState::Following,
        Ok(false) => FollowState::NotFollowing,
        Err(_) => FollowState::Unknown,
    };
    DetailPayload {
        illust,
        pages,
        pages_failed,
        following,
    }
}

/// How many cells the viewer draws for one payload.
///
/// The payload's own count is the floor: a work Pixiv says has 30 pages draws 30
/// cells even when the page list never arrived (T1).
pub(crate) fn viewer_page_count(page_count: u32, pages: usize) -> usize {
    (page_count.max(1) as usize).max(pages)
}

/// The sentence the page carries for whatever did not arrive (T1).
pub(crate) fn detail_note(pages_failed: bool, following: FollowState) -> Option<String> {
    let mut notes: Vec<&'static str> = Vec::new();
    if pages_failed {
        notes.push("分页列表没有读到：这里按作品标称的页数画出占位，可能只有第一页有图");
    }
    if following == FollowState::Unknown {
        notes.push("关注状态未知：画师资料没有读到，这里不会显示你是否已关注");
    }
    if notes.is_empty() {
        None
    } else {
        Some(notes.join("；"))
    }
}

impl App {
    /// Put one work on the detail page.
    ///
    /// The follow state is an enum now: `false` used to mean both 「未关注」 and
    /// 「没读到」 (T1).
    pub(crate) fn install_detail_with_follow(&mut self, payload: DetailPayload, epoch: u64) {
        if self.detail_epoch != epoch {
            return;
        }
        let DetailPayload {
            illust,
            pages,
            pages_failed,
            following,
        } = payload;

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
            // F4: the restriction the *reply* reported, never one inferred from a
            // click. A private bookmark draws its lock glyph from this alone.
            ui.set_bookmark_restrict(restrict_code(illust.bookmark.restrict));
            // The button's label is a claim about the artist, so it is only drawn
            // from a read that actually answered (T1).
            ui.set_detail_following(following.known().unwrap_or(false));
            ui.set_detail_message(
                detail_note(pages_failed, following)
                    .unwrap_or_default()
                    .into(),
            );
            ui.set_detail_ugoira_hint(
                if illust.kind == IllustKind::Ugoira {
                    "正在准备内置播放；也能导出为 GIF 保存在下载目录。"
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

        // F2: `is_masked` is read once, here, because it decides two things — what
        // the plan may request and what the page draws.
        let masked = illust.is_masked;
        // Page skeletons, sized from the real page metadata when we have it.
        let count = viewer_page_count(illust.page_count, pages.len());
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
        self.detail_masked = masked;
        let ui = self.ui();
        ui.set_detail_pages(ModelRc::from(models.clone()));
        ui.set_detail_masked(masked);
        ui.set_detail_sensitivity(sensitivity_label(&illust).into());
        ui.set_detail_ai(illust.ai_type != 0);
        ui.set_detail_state(0);

        // ---- O1: decode what is shown, and only what is shown -------------
        // The strip draws 48 px cells and the viewer draws the current page plus
        // its neighbours. Before this, every page was decoded at viewer size and
        // the thumbnails reused that bitmap: a 30-page work paid 30 large decodes
        // for the one picture on screen. The plan comes from `detail_plan`, so the
        // counting test and the running app agree on what "one decode" means.
        let seed = illust.id;
        // F2: a work Pixiv marks sensitive is drawn blurred behind 「显示」, and the
        // blur is the small tier — `largest()` is the only call that reaches the
        // original, so a masked page hands the plan the master1200 fallback and lets
        // `masked_detail_plan` keep the viewer on the small tier as well.
        let largest = if masked {
            illust.urls.viewer().to_string()
        } else {
            illust.urls.largest().to_string()
        };
        let requests = if masked {
            masked_detail_plan(&pages, &largest, 0)
        } else {
            detail_plan(&pages, &largest, 0)
        };

        self.detail = Some(illust);
        self.detail_pages = pages;
        self.detail_models = Some(models.clone());
        self.spawn_detail_decodes(requests, epoch, seed);

        // F1: a ugoira gets its film decoded once, at the width it is drawn.
        if let Some(illust) = self
            .detail
            .as_ref()
            .filter(|d| d.kind == IllustKind::Ugoira)
        {
            let (width, height) = (illust.width, illust.height);
            self.load_film(seed, width, height);
        }

        // Only now, with this page owning the work: the bounded id lookup reads
        // `self.detail`, so asking before the move resolves the *previous* page's
        // author, which is what the first attempt did.
        match self.detail_author_avatar_url.clone() {
            Some(url) => self.spawn_avatars(vec![url]),
            None => self.backfill_author_avatars(),
        }
        // A partly loaded page says which half is missing, above the shell's status
        // bar as well as in `detail-message`, because that property is only drawn in
        // the loading and error states (T1).
        if let Some(note) = detail_note(pages_failed, following) {
            self.notify(2, note);
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

        // A masked page has not asked for the original and must not start now.
        let fallback = if self.detail_masked {
            illust.urls.viewer()
        } else {
            illust.urls.largest()
        };
        let Some(url) = retry_url(&self.detail_pages, fallback, index) else {
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
        let rt = self.tasks.clone();
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
                        // The strip's bitmap is a separate 48 px decode now, so a
                        // viewer retry must not overwrite it with a 1600 px one.
                        page.source = raw.to_slint();
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

/// A toggle that is already in flight owns its button; the second click is dropped.
///
/// Two clicks used to send two requests, each computed from the same stale state,
/// so the second undid the first and the page kept whichever reply landed last
/// (T13). `toggle_follow` has always held this slot; the bookmark toggle did not.
pub(crate) fn toggle_allowed(busy: bool) -> bool {
    !busy
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
                self.patch_bookmark_card(illust_id, bookmarked);
            }
            Mode::Real => {
                if !self.session.is_authenticated() {
                    self.notify(2, "需要登录后才能收藏，请到设置页登录");
                    return;
                }
                let Some(client) = self.client.clone() else {
                    return;
                };
                // One request at a time, decided once: the busy flag the follow
                // button uses is the same slot, so a double click sends one POST and
                // the reply it draws belongs to the state this click saw (T13).
                {
                    let ui = self.ui();
                    if !toggle_allowed(ui.get_detail_busy()) {
                        return;
                    }
                    ui.set_detail_busy(true);
                }
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
                let rt = self.tasks.clone();
                rt.spawn(async move {
                    let result = client.set_bookmark(illust_id, next, restrict).await;
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        // The slot is free again whatever the answer was (T13).
                        app.ui().set_detail_busy(false);
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

/// Write one card's `bookmarked` flag, if it disagrees. Returns whether it wrote.
///
/// This is the whole of a bookmark publish now (B3). `resync_bookmarks` walked every
/// column, read every row and wrote back every row whose flag differed, and
/// `apply_batch_bookmarks` called it once per work: fifty works over five hundred
/// rows was ~25 000 row clones on the UI thread inside one reply, and every patch
/// that *did* write dropped that row's hover and focus. A verdict names one work, so
/// one row is what the feed is asked for.
pub(crate) fn patch_card_bookmark<M: Model<Data = IllustItem>>(
    model: &M,
    row: usize,
    bookmarked: bool,
) -> bool {
    let Some(mut item) = model.row_data(row) else {
        return false;
    };
    if item.bookmarked == bookmarked {
        return false;
    }
    item.bookmarked = bookmarked;
    model.set_row_data(row, item);
    true
}

impl App {
    /// Write one card's bookmark flag, addressed by the cell the feed already holds.
    pub(crate) fn patch_bookmark_card(&self, id: Id, bookmarked: bool) -> bool {
        let Some((column, row)) = self.index.get(&id.to_string()).copied() else {
            return false;
        };
        let Some(model) = self.columns.get(column) else {
            return false;
        };
        patch_card_bookmark(model, row, bookmarked)
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
            // F4: the menu and the lock glyph follow the reply as well, so a
            // refused re-tag leaves the old restriction on screen instead of a
            // menu that claims a state the server never accepted.
            ui.set_bookmark_restrict(restrict_code(state.restrict));
        }
        // One card, not the feed (B3).
        self.patch_bookmark_card(id, state.bookmarked);
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
        // The work and the epoch the reply will be an answer about (B4).
        let work = illust.id;
        let epoch = self.detail_epoch;
        self.ui().set_detail_busy(true);
        let rt = self.tasks.clone();
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
                // A reply about another work, or about a page that has since moved
                // on, writes nothing at all (B4); the open that replaced this page
                // already cleared the slot it took.
                if !detail_reply_lands(
                    app.detail_epoch,
                    epoch,
                    app.detail.as_ref().map(|detail| detail.id),
                    work,
                ) {
                    return;
                }
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

// ------------------------------------------------------- O1: decode what is shown

/// How large a decode the detail page is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DecodeSize {
    /// The 48 px strip cell.
    Thumb,
    /// The viewer, at its own cap.
    Viewer,
}

/// One decode request, at the size the picture is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecodeRequest {
    /// Page index this request fills in.
    pub index: usize,
    pub url: String,
    pub size: DecodeSize,
}

/// The strip's cells are 48 logical px (ui/pages/detail.slint draws each at
/// 48 x 48), so the decode target is 48 x the display scale rather than the
/// viewer's width.
pub(crate) const THUMB_LOGICAL_PX: f32 = 48.0;
/// Ceiling for a strip thumbnail: 48 logical at 300 % is 144, rounded up to a
/// tier by target_width_up_to.
pub(crate) const THUMB_MAX_WIDTH: u32 = 256;
/// How many pages on each side of the current one get a viewer-sized decode.
pub(crate) const VIEWER_RADIUS: usize = 1;
/// Decodes in flight for one detail page. Three matches the buffer_unordered(3)
/// that preceded this plan; it also bounds the strip of a 30-page work.
const DETAIL_DECODE_CONCURRENCY: usize = 3;

/// The strip URL for one page.
fn page_url(page: &PageMeta, size: DecodeSize) -> &str {
    let (primary, fallback) = match size {
        DecodeSize::Thumb => (&page.urls.small, &page.urls.regular),
        DecodeSize::Viewer => (&page.urls.regular, &page.urls.small),
    };
    if primary.is_empty() {
        fallback.as_str()
    } else {
        primary.as_str()
    }
}

/// Every decode the detail page needs, for the page the viewer is on.
///
/// One request per page for the strip, plus a viewer request for the current page
/// and its neighbours - the only pictures actually drawn. The largest argument
/// covers a work whose page list came back empty: it has exactly one image and it
/// is both the strip cell and the viewer.
///
/// A URL is requested at most once per size, so two pages that share a URL (a
/// crafted payload, or the single-image fallback) cannot double a decode.
pub(crate) fn detail_plan(pages: &[PageMeta], largest: &str, current: usize) -> Vec<DecodeRequest> {
    if pages.is_empty() {
        if largest.is_empty() {
            return Vec::new();
        }
        return vec![
            DecodeRequest {
                index: 0,
                url: largest.to_string(),
                size: DecodeSize::Thumb,
            },
            DecodeRequest {
                index: 0,
                url: largest.to_string(),
                size: DecodeSize::Viewer,
            },
        ];
    }

    let mut requests = Vec::with_capacity(pages.len() + VIEWER_RADIUS * 2 + 1);
    let mut seen: HashSet<String> = HashSet::new();
    for (index, page) in pages.iter().enumerate() {
        let url = page_url(page, DecodeSize::Thumb);
        if url.is_empty() || !seen.insert(url.to_string()) {
            continue;
        }
        requests.push(DecodeRequest {
            index,
            url: url.to_string(),
            size: DecodeSize::Thumb,
        });
    }

    let last = pages.len() - 1;
    let current = current.min(last);
    let first = current.saturating_sub(VIEWER_RADIUS);
    let end = current.saturating_add(VIEWER_RADIUS).min(last);
    let mut seen: HashSet<String> = HashSet::new();
    for index in first..=end {
        let Some(page) = pages.get(index) else {
            continue;
        };
        let url = page_url(page, DecodeSize::Viewer);
        if url.is_empty() || !seen.insert(url.to_string()) {
            continue;
        }
        requests.push(DecodeRequest {
            index,
            url: url.to_string(),
            size: DecodeSize::Viewer,
        });
    }
    requests
}

/// The sensitivity badge's text, from Pixiv's own two fields.
///
/// `x_restrict` is the age rating the search and detail payloads both carry, and
/// `sl` / `is_masked` are the newer sensitivity marks: a work can be marked without
/// being R-18. Empty means "Pixiv marks nothing", which is what keeps the badge off
/// the toolbar instead of drawing a chip that says "normal".
pub(crate) fn sensitivity_label(illust: &Illust) -> String {
    match illust.x_restrict {
        1 => "R-18".to_string(),
        2 => "R-18G".to_string(),
        _ if illust.is_masked || illust.sl > 0 => "敏感".to_string(),
        _ => String::new(),
    }
}

/// The decode plan a work Pixiv marks sensitive may run, before 「显示」.
///
/// The web draws such a work blurred and serves the blur from the *small* tier; the
/// viewer here asked for the master1200 tier, so a masked page asks for the small one
/// instead and lifts it only when the mask is lifted. The one URL this app can reach
/// that carries the whole picture — `urls.original`, which `largest()` prefers — is
/// therefore never requested while the mask is up.
pub(crate) fn masked_detail_plan(
    pages: &[PageMeta],
    largest: &str,
    current: usize,
) -> Vec<DecodeRequest> {
    detail_plan(pages, largest, current)
        .into_iter()
        .map(|request| match request.size {
            DecodeSize::Viewer => match pages.get(request.index) {
                // No page metadata: the single-image fallback is the caller's URL,
                // which under a mask is already the viewer tier.
                None => request,
                Some(page) => DecodeRequest {
                    url: page_url(page, DecodeSize::Thumb).to_string(),
                    ..request
                },
            },
            DecodeSize::Thumb => request,
        })
        .collect()
}

impl App {
    /// 「显示」: the mask is lifted, and only now is the full-size page asked for.
    pub(crate) fn reveal_masked(&mut self) {
        if !self.detail_masked {
            return;
        }
        self.detail_masked = false;
        self.ui().set_detail_masked(false);
        let Some(illust) = self.detail.clone() else {
            return;
        };
        let pages = self.detail_pages.clone();
        let current = self.ui().get_detail_page_index().max(0) as usize;
        let seed = illust.id;
        let epoch = self.detail_epoch;
        let requests = detail_plan(&pages, illust.urls.largest(), current);
        self.spawn_detail_decodes(requests, epoch, seed);
    }
}

/// Adopt one decode result into the page model.
///
/// The epoch_current flag is (detail_epoch == epoch) read at the moment the
/// result landed: a decode that was in flight when the work or the page changed is
/// dropped here, whole, rather than painted into a viewer that has moved on.
/// Returns whether the row was touched, so a caller can tell adopt from drop.
pub(crate) fn adopt_decode(
    model: &VecModel<ViewerPage>,
    request: &DecodeRequest,
    image: Option<slint::Image>,
    epoch_current: bool,
) -> bool {
    if !epoch_current {
        return false;
    }
    let Some(mut page) = model.row_data(request.index) else {
        return false;
    };
    match (request.size, image) {
        (DecodeSize::Viewer, Some(image)) => {
            page.source = image;
            page.loaded = true;
            page.failed = false;
        }
        (DecodeSize::Viewer, None) => {
            page.loaded = false;
            page.failed = true;
        }
        // A missing thumbnail keeps the placeholder: the viewer's own failure
        // path is what reports a broken page, and saying it twice helps nobody.
        (DecodeSize::Thumb, Some(image)) => page.thumb = image,
        (DecodeSize::Thumb, None) => return false,
    }
    model.set_row_data(request.index, page);
    true
}

/// U29: the page a step lands on, or None when it would leave the work.
///
/// The clamp is the rule: at the first page a -1 does nothing at all rather than
/// wrapping to the last one, because a wrap is a page change the user did not ask
/// for.
pub(crate) fn stepped_page(current: i32, delta: i32, count: usize) -> Option<i32> {
    if count == 0 {
        return None;
    }
    let last = i32::try_from(count - 1).unwrap_or(i32::MAX);
    let next = current.saturating_add(delta).clamp(0, last);
    (next != current).then_some(next)
}

/// F4: the UI's 0 / 1 encoding of a bookmark's visibility.
pub(crate) fn restrict_code(restrict: BookmarkRestrict) -> i32 {
    match restrict {
        BookmarkRestrict::Private => 1,
        BookmarkRestrict::Public => 0,
    }
}

/// The restriction a menu row asked for.
pub(crate) fn restrict_from_code(code: i32) -> BookmarkRestrict {
    if code == 1 {
        BookmarkRestrict::Private
    } else {
        BookmarkRestrict::Public
    }
}

impl App {
    /// Decode width for one strip cell at the live display scale.
    fn detail_thumb_target(&self) -> u32 {
        let Some(ui) = self.ui.upgrade() else {
            return THUMB_MAX_WIDTH;
        };
        let scale = ui.window().scale_factor().max(1.0);
        crate::images::target_width_up_to(THUMB_LOGICAL_PX, scale, THUMB_MAX_WIDTH)
    }

    /// Run a plan off the UI thread and adopt each result that is still current.
    fn spawn_detail_decodes(&self, requests: Vec<DecodeRequest>, epoch: u64, seed: Id) {
        if requests.is_empty() {
            return;
        }
        let mode = self.mode;
        let cache = self.cache.clone();
        let rt = self.tasks.clone();
        let thumb_target = self.detail_thumb_target();
        rt.spawn(async move {
            let mut stream = futures::stream::iter(requests)
                .map(move |request| {
                    let cache = cache.clone();
                    async move {
                        let raw = if mode == Mode::Demo {
                            demo_page(seed, &request, thumb_target).await
                        } else {
                            match request.size {
                                DecodeSize::Thumb => {
                                    fetch_sized(&cache, &request.url, thumb_target).await
                                }
                                DecodeSize::Viewer => {
                                    fetch_sized_viewer(&cache, &request.url).await
                                }
                            }
                        };
                        (request, raw)
                    }
                })
                .buffer_unordered(DETAIL_DECODE_CONCURRENCY);

            while let Some((request, raw)) = stream.next().await {
                on_ui(move |shared| {
                    let Ok(app) = shared.try_borrow() else {
                        return;
                    };
                    let Some(model) = app.detail_models.clone() else {
                        return;
                    };
                    // Read the epoch here, on the UI thread, at the instant the
                    // result is adopted: comparing it in the task would race the
                    // navigation it is supposed to lose to.
                    let current = app.detail_epoch == epoch;
                    adopt_decode(&model, &request, raw.map(|raw| raw.to_slint()), current);
                });
            }
        });
    }

    /// Show page index, clamped, and plan the decodes around it.
    pub(crate) fn show_page(&mut self, index: i32) {
        let Some(model) = self.detail_models.clone() else {
            return;
        };
        let count = model.row_count();
        if count == 0 {
            return;
        }
        let next = usize::try_from(index.max(0)).unwrap_or(0).min(count - 1);
        let next = i32::try_from(next).unwrap_or(0);
        let moved = {
            let ui = self.ui();
            let moved = ui.get_detail_page_index() != next;
            ui.set_detail_page_index(next);
            ui.set_detail_zoom(1.0);
            ui.set_detail_pan_x(0.0);
            ui.set_detail_pan_y(0.0);
            ui.set_detail_fit_mode(true);
            moved
        };
        if !moved {
            // Re-selecting the page already on screen still resets the view, as
            // it always has; it just has nothing new to decode.
            return;
        }
        // The viewer only ever holds current +/- 1, so moving one page needs one
        // new decode, not a second pass over the work.
        self.request_detail_viewer(usize::try_from(next).unwrap_or(0));
    }

    /// U29: left/right on a thumbnail. Out of range does nothing.
    pub(crate) fn page_step(&mut self, delta: i32) {
        let current = self.ui().get_detail_page_index();
        let count = self
            .detail_models
            .as_ref()
            .map_or(0, |model| model.row_count());
        if let Some(next) = stepped_page(current, delta, count) {
            self.show_page(next);
        }
    }

    /// Ask for the viewer-sized decode of the pages around the current one.
    fn request_detail_viewer(&mut self, current: usize) {
        let Some(model) = self.detail_models.clone() else {
            return;
        };
        let largest = self
            .detail
            .as_ref()
            .map(|detail| detail.urls.largest().to_string())
            .unwrap_or_default();
        let pages = self.detail_pages.clone();
        let requests: Vec<DecodeRequest> = detail_plan(&pages, &largest, current)
            .into_iter()
            .filter(|request| request.size == DecodeSize::Viewer)
            // A page that already holds its bitmap is not asked for again:
            // stepping back and forth must not re-decode what is on screen.
            .filter(|request| {
                model
                    .row_data(request.index)
                    .is_some_and(|page| !page.loaded || page.failed)
            })
            .collect();
        if requests.is_empty() {
            return;
        }
        let seed = self.detail.as_ref().map_or(Id(0), |detail| detail.id);
        self.spawn_detail_decodes(requests, self.detail_epoch, seed);
    }

    /// F1: fetch, decode and publish the film for the work just opened.
    fn load_film(&mut self, id: Id, width: u32, height: u32) {
        self.release_playback();
        let epoch = self.detail_epoch;
        // Tag this film's generation: a tick or a decode that lands after the
        // page has moved on compares against it and stops.
        self.playback.epoch = epoch;
        let mode = self.mode;
        let rt = self.tasks.clone();
        let client = self.client.clone();
        let target = film_width_for(&self.ui());
        rt.spawn(async move {
            let result = load_film_bytes(mode, client, id, width, height, target).await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                if app.detail_epoch != epoch || app.playback.epoch != epoch {
                    return;
                }
                match result {
                    Ok(film) => app.publish_film(film),
                    Err(message) => {
                        // Every failure keeps the still and says why: a silent
                        // film that never plays is the worst of the three.
                        app.notify(3, message);
                    }
                }
            });
        });
    }

    /// F4: re-tag the current work's bookmark, or bookmark it, with the code the
    /// menu sent (0 public / 1 private).
    ///
    /// set_bookmark(id, true, restrict) is both verbs: the API treats a bookmark
    /// that already exists as "move it to this restriction", which is why choosing
    /// a row in the menu is one call and never a remove-then-add.
    pub(crate) fn pick_restrict(&mut self, code: i32) {
        let restrict = restrict_from_code(code);
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let illust_id = detail.id;

        if self.mode == Mode::Demo {
            if let Some(item) = self.items.iter_mut().find(|item| item.id == illust_id) {
                item.apply_bookmark(true, restrict);
            }
            if let Some(detail) = self.detail.as_mut().filter(|d| d.id == illust_id) {
                detail.apply_bookmark(true, restrict);
            }
            {
                let ui = self.ui();
                ui.set_detail_bookmarked(true);
                ui.set_bookmark_restrict(restrict_code(restrict));
            }
            self.notify(
                2,
                if restrict == BookmarkRestrict::Private {
                    "演示模式：已设为仅自己可见（只在本机）"
                } else {
                    "演示模式：已设为公开收藏（只在本机）"
                },
            );
            self.patch_bookmark_card(illust_id, true);
            return;
        }

        if !self.session.is_authenticated() {
            self.notify(2, "需要登录后才能收藏，请到设置页登录");
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        // The star and the menu share one slot (B5). This path used to bypass both
        // the guard and the flag, so the menu and the star could both be in flight
        // and the later reply won invisibly - the same defect T13 fixed for the star
        // alone, in the one place that skipped it.
        {
            let ui = self.ui();
            if !toggle_allowed(ui.get_detail_busy()) {
                return;
            }
            ui.set_detail_busy(true);
        }
        let epoch = self.detail_epoch;
        let work = illust_id;
        let rt = self.tasks.clone();
        rt.spawn(async move {
            let result = client.set_bookmark(illust_id, true, restrict).await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                // The verdict belongs to the page that asked (B5, same rule as B4);
                // a page that moved on has already freed the slot it took.
                if !detail_reply_lands(
                    app.detail_epoch,
                    epoch,
                    app.detail.as_ref().map(|detail| detail.id),
                    work,
                ) {
                    return;
                }
                app.ui().set_detail_busy(false);
                match result {
                    Ok(state) => {
                        // The verdict, not the request: a refused re-tag leaves
                        // the menu on the restriction the server still reports.
                        let private = restrict_code(state.restrict) == 1;
                        app.apply_bookmark_state(illust_id, state);
                        app.notify(
                            1,
                            if private {
                                "已收藏（仅自己可见）"
                            } else {
                                "已收藏（公开）"
                            },
                        );
                    }
                    Err(error) => app.notify(3, format!("收藏失败：{error}")),
                }
            });
        });
    }
}

/// Decode one page of the demo catalogue at the size the plan asked for.
async fn demo_page(seed: Id, request: &DecodeRequest, thumb_target: u32) -> Option<RawImage> {
    let id = Id(seed.get().wrapping_add(request.index as u64));
    let page = u64::try_from(request.index).unwrap_or(0);
    match request.size {
        DecodeSize::Thumb => {
            let side = thumb_target.max(64);
            demo_raw(id, side, side, page).await
        }
        DecodeSize::Viewer => demo_raw(id, 900, 1200, page).await,
    }
}

/// Fetch and decode a film, off the UI thread, at target pixels wide.
///
/// The three failures the plan names arrive here as one sentence each: a 404 or an
/// unauthorised archive is the PixivError text, a frame count that disagrees with
/// the metadata is UgoiraError::FrameMismatch, and a damaged archive is the decode
/// error.
async fn load_film_bytes(
    mode: Mode,
    client: Option<Client>,
    id: Id,
    width: u32,
    height: u32,
    target: u32,
) -> Result<UgoiraFilm, String> {
    match mode {
        Mode::Demo => {
            let film = tokio::task::spawn_blocking(move || demo_film(id, width, height))
                .await
                .map_err(|error| format!("演示动图生成失败：{error}"))?;
            film.ok_or_else(|| "演示动图生成失败：帧无法渲染，保持静帧显示".to_string())
        }
        Mode::Real => {
            let Some(client) = client else {
                return Err("没有可用的网络客户端，动图无法播放".to_string());
            };
            let meta = client
                .ugoira_meta(id)
                .await
                .map_err(|error| format!("动图元数据不可用，保持静帧显示：{error}"))?;
            let zip_url = meta.best_zip().to_string();
            let (bytes, _) = client
                .fetch_bytes(&zip_url)
                .await
                .map_err(|error| format!("动图压缩包下载失败，保持静帧显示：{error}"))?;
            let frames = meta.frames;
            let decoded = tokio::task::spawn_blocking(move || {
                UgoiraFilm::decode_zip(&bytes, &frames).map(|film| {
                    // scaled_to_width clones when it has nothing to do, and that
                    // clone doubles the film's peak for a film that is already
                    // small enough to draw.
                    if target < film.width {
                        film.scaled_to_width(target)
                    } else {
                        film
                    }
                })
            })
            .await
            .map_err(|error| format!("动图解码任务失败，保持静帧显示：{error}"))?;
            decoded.map_err(|error| format!("动图无法播放，保持静帧显示：{error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    // ---------------------------------------------------------------- F2: the mask

    fn page(index: u32, small: &str, regular: &str, original: &str) -> PageMeta {
        PageMeta {
            index,
            width: 600,
            height: 800,
            urls: PageUrls {
                thumb_mini: format!("{small}-thumb-mini"),
                small: small.to_string(),
                regular: regular.to_string(),
                original: original.to_string(),
            },
        }
    }

    /// A work with the three fields the mask reads; everything else is the shape a
    /// payload with no optional extras produces.
    fn sensitive(x_restrict: u8, sl: u8, is_masked: bool) -> Illust {
        Illust {
            id: Id(1),
            title: String::new(),
            kind: IllustKind::Illustration,
            page_count: 1,
            width: 600,
            height: 800,
            urls: IllustUrls::default(),
            author: Author::default(),
            tags: Vec::new(),
            description: String::new(),
            create_date: None,
            bookmark: Bookmark::default(),
            stats: IllustStats::default(),
            x_restrict,
            ai_type: 0,
            alt: String::new(),
            sl,
            is_masked,
            is_bookmarkable: true,
        }
    }

    /// The badge says the level, and says nothing when Pixiv marks nothing.
    #[test]
    fn f2_the_sensitivity_badge_says_the_level_and_stays_off_for_an_unmarked_work() {
        assert_eq!(sensitivity_label(&sensitive(1, 0, false)), "R-18");
        assert_eq!(sensitivity_label(&sensitive(2, 2, true)), "R-18G");
        assert_eq!(
            sensitivity_label(&sensitive(0, 1, false)),
            "敏感",
            "a work can be marked sensitive without an age rating"
        );
        assert_eq!(sensitivity_label(&sensitive(0, 0, true)), "敏感");
        assert_eq!(
            sensitivity_label(&sensitive(0, 0, false)),
            "",
            "an unmarked work draws no chip: normal is not a level"
        );
    }

    /// A masked page asks for the small tier and never for the original.
    #[test]
    fn f2_a_masked_page_never_requests_the_original() {
        let pages: Vec<PageMeta> = (0..3)
            .map(|index| {
                page(
                    index,
                    &format!("https://i.pximg.net/c/360x360/small-{index}.jpg"),
                    &format!("https://i.pximg.net/c/1200x1200/regular-{index}.jpg"),
                    &format!("https://i.pximg.net/img-original/original-{index}.png"),
                )
            })
            .collect();
        let masked = masked_detail_plan(&pages, "https://i.pximg.net/img-original/f1.png", 1);
        assert!(
            !masked
                .iter()
                .any(|request| request.url.contains("img-original")),
            "a masked page must not fetch the full-size image: {:?}",
            masked
        );
        for request in masked.iter().filter(|r| r.size == DecodeSize::Viewer) {
            assert!(
                request.url.contains("small-"),
                "the viewer asks for the small tier while the mask is up: {:?}",
                request
            );
        }
        // The strip is untouched: it was already the small tier.
        assert!(
            masked
                .iter()
                .any(|request| request.size == DecodeSize::Thumb),
            "the strip keeps its cells"
        );
        // The unmasked plan is the one that reaches for the bigger copy, so the
        // difference above is the mask and not the plan.
        let plain = detail_plan(&pages, "https://i.pximg.net/img-original/f1.png", 1);
        assert!(
            plain.iter().any(|request| request.url.contains("regular-")),
            "the revealed page asks for the master1200 tier"
        );
    }

    /// No page metadata: the caller's fallback is the only URL, and under a mask the
    /// caller hands the viewer tier (never `largest()`).
    #[test]
    fn f2_the_single_image_fallback_is_the_viewer_tier_while_masked() {
        let illust = sensitive(1, 1, true);
        let urls = IllustUrls {
            regular: "https://i.pximg.net/c/1200x1200/regular.jpg".to_string(),
            original: "https://i.pximg.net/img-original/original.png".to_string(),
            ..IllustUrls::default()
        };
        let illust = Illust { urls, ..illust };
        assert!(illust.urls.largest().contains("img-original"));
        assert!(illust.urls.viewer().contains("regular"));
        // What `install_detail_with_follow` does with the flag:
        let fallback = if illust.is_masked {
            illust.urls.viewer().to_string()
        } else {
            illust.urls.largest().to_string()
        };
        let plan = masked_detail_plan(&[], &fallback, 0);
        assert!(
            !plan
                .iter()
                .any(|request| request.url.contains("img-original")),
            "the fallback a masked page passes must not be the original: {plan:?}"
        );
    }

    use super::*;
    use pixiv_types::domain::PageUrls;

    /// B5 · the visibility menu and the star take the same busy slot.
    ///
    /// `pick_restrict` consulted neither `toggle_allowed` nor `detail-busy`, so a menu
    /// row clicked while the star's POST was in flight sent a second request and the
    /// later reply decided the button. The plant that makes this red is
    /// [`toggle_allowed`] answering `true` while busy - "the slot is not consulted".
    #[test]
    fn b5_the_visibility_menu_and_the_star_take_one_slot() {
        // The sequence both gestures now follow: ask the slot, take it, free it when
        // the reply lands.
        let mut busy = false;
        assert!(toggle_allowed(busy), "the star's click starts");
        busy = true;
        assert!(
            !toggle_allowed(busy),
            "the menu row clicked while the star is in flight is dropped, not raced"
        );
        busy = false;
        assert!(
            toggle_allowed(busy),
            "the next gesture starts once the reply has landed"
        );
    }

    /// B4 · A's follow reply must not rewrite B's button.
    ///
    /// `toggle_follow`'s landing compared neither the epoch nor the route, so a slow
    /// follow of work A set `detail-following` on whatever work was open when the
    /// answer arrived. The plant that makes this red is [`detail_reply_lands`]
    /// answering `true` for every reply - the rule it had.
    #[test]
    fn b4_a_follow_reply_lands_only_on_the_work_that_asked() {
        let asked = Id(11);
        assert!(
            detail_reply_lands(7, 7, Some(asked), asked),
            "same page, same work"
        );
        assert!(
            !detail_reply_lands(7, 7, Some(Id(12)), asked),
            "same epoch is not enough: the page now shows another work"
        );
        assert!(
            !detail_reply_lands(8, 7, Some(asked), asked),
            "the page moved on while the follow was in flight"
        );
        assert!(
            !detail_reply_lands(7, 7, None, asked),
            "no work on screen is nobody's reply"
        );
    }

    /// B3 · a batch of verdicts patches the cards it names, not the feed.
    ///
    /// `apply_batch_bookmarks` called `resync_bookmarks` once per work, and that
    /// walked every column, read every row and rewrote every row whose flag
    /// differed: fifty works over five hundred rows is 25 000 reads on the UI thread
    /// inside one reply, plus a lost hover on every row it did touch. The plant that
    /// makes this red is [`patch_card_bookmark`] walking the model the way
    /// `resync_bookmarks` did.
    #[test]
    fn b3_a_batch_of_fifty_verdicts_patches_fifty_rows() {
        use crate::app::helpers::testkit::{CountingModel, bare_card};

        const ROWS: usize = 500;
        const BATCH: usize = 50;
        let model = CountingModel::new(
            (0..ROWS)
                .map(|index| bare_card(&format!("id{index}")))
                .collect::<Vec<_>>(),
        );

        // The batch: fifty verdicts, each naming one work the feed shows.
        let mut wrote = 0;
        for index in 0..BATCH {
            if patch_card_bookmark(&model, index, true) {
                wrote += 1;
            }
        }
        assert_eq!(model.reads(), BATCH, "one read per changed card");
        assert_eq!(model.writes(), BATCH, "one write per changed card");
        assert_eq!(wrote, BATCH, "every verdict changed its own card");

        // A verdict that changes nothing writes nothing at all.
        model.reset();
        assert!(
            !patch_card_bookmark(&model, 0, true),
            "the flag already says so"
        );
        assert_eq!(model.writes(), 0);
        assert_eq!(model.reads(), 1, "one row is read to know that");

        // ...and the shape this replaced, measured in the same test so the claim is a
        // number rather than a sentence: the old path read every row, per verdict.
        model.reset();
        for _ in 0..BATCH {
            for row in 0..ROWS {
                let Some(mut item) = model.row_data(row) else {
                    continue;
                };
                if !item.bookmarked {
                    item.bookmarked = true;
                    model.set_row_data(row, item);
                }
            }
        }
        assert_eq!(
            model.reads(),
            ROWS * BATCH,
            "the old resync read every row of every column, once per work"
        );
    }

    /// B1 · a failed read belongs to the page that asked for it.
    ///
    /// The success path has compared `detail_epoch` since round six; the failure path
    /// wrote `detail-state = 3` with no epoch at all, so a slow failed load of A
    /// replaced the already-loaded B with A's error. The plant that makes this red is
    /// the rule as it was - [`reply_lands`] answering `true` for every pair.
    #[test]
    fn b1_a_failed_read_lands_only_on_the_page_that_asked() {
        assert!(reply_lands(
            ReplySurface::Detail(7),
            ReplySurface::Detail(7)
        ));
        assert!(
            !reply_lands(ReplySurface::Detail(8), ReplySurface::Detail(7)),
            "B opened while A's read was in flight: A's error must not land"
        );
        assert!(
            !reply_lands(ReplySurface::Profile(3), ReplySurface::Detail(7)),
            "the user page is not the illustration page"
        );
    }

    fn synthetic_page(index: usize) -> PageMeta {
        PageMeta {
            index: u32::try_from(index).unwrap_or(0),
            width: 1200,
            height: 1600,
            urls: PageUrls {
                thumb_mini: format!("https://i.pximg.net/c/48x48/p{index}_square1200.jpg"),
                small: format!("https://i.pximg.net/c/540x540/p{index}_master1200.jpg"),
                regular: format!("https://i.pximg.net/img-master/p{index}_p0_master1200.jpg"),
                original: format!("https://i.pximg.net/img-original/p{index}_p0.jpg"),
            },
        }
    }

    /// A cache that decodes nothing and counts what it was asked for.
    ///
    /// The defect O1 fixes is a number of decodes, and a real ImageCache cannot be
    /// asserted on without a temp directory and a decoder. This fake implements
    /// the only part of the contract the plan depends on: one request in, one
    /// decode out, at the size named.
    #[derive(Default)]
    struct CountingCache {
        calls: Vec<(String, DecodeSize)>,
    }

    impl CountingCache {
        fn run(&mut self, plan: &[DecodeRequest]) {
            for request in plan {
                self.calls.push((request.url.clone(), request.size));
            }
        }

        fn count(&self, size: DecodeSize) -> usize {
            self.calls.iter().filter(|(_, at)| *at == size).count()
        }

        fn once_per_url(&self) -> bool {
            let mut seen = HashSet::new();
            self.calls
                .iter()
                .all(|(url, size)| seen.insert((url.clone(), *size)))
        }
    }

    #[test]
    fn thirty_pages_decode_three_large_images_and_thirty_thumbnails() {
        let pages: Vec<PageMeta> = (0..30).map(synthetic_page).collect();
        let plan = detail_plan(&pages, "https://i.pximg.net/img-original/largest.jpg", 15);
        let mut cache = CountingCache::default();
        cache.run(&plan);

        assert_eq!(
            cache.count(DecodeSize::Viewer),
            3,
            "current and both neighbours"
        );
        assert_eq!(
            cache.count(DecodeSize::Thumb),
            30,
            "one strip cell per page"
        );
        assert_eq!(cache.calls.len(), 33, "nothing else is decoded");
        assert!(cache.once_per_url(), "no URL is decoded twice");

        let mut large: Vec<usize> = plan
            .iter()
            .filter(|request| request.size == DecodeSize::Viewer)
            .map(|request| request.index)
            .collect();
        large.sort_unstable();
        assert_eq!(large, vec![14, 15, 16]);
    }

    #[test]
    fn the_viewer_decodes_only_what_the_edges_allow() {
        let pages: Vec<PageMeta> = (0..30).map(synthetic_page).collect();
        let first = detail_plan(&pages, "", 0);
        assert_eq!(
            first
                .iter()
                .filter(|r| r.size == DecodeSize::Viewer)
                .count(),
            2,
            "the first page has one neighbour"
        );
        let last = detail_plan(&pages, "", 29);
        assert_eq!(
            last.iter().filter(|r| r.size == DecodeSize::Viewer).count(),
            2,
            "the last page has one neighbour"
        );
        let stale = detail_plan(&pages, "", 900);
        assert!(
            stale.iter().all(|request| request.index < pages.len()),
            "every request points at a real page"
        );
    }

    #[test]
    fn a_single_page_and_an_empty_page_list_both_decode_once_per_size() {
        let one = vec![synthetic_page(0)];
        let plan = detail_plan(&one, "https://i.pximg.net/img-original/fallback.jpg", 0);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].size, DecodeSize::Thumb);
        assert_eq!(plan[1].size, DecodeSize::Viewer);

        let empty = detail_plan(&[], "https://i.pximg.net/img-original/only.jpg", 0);
        assert_eq!(
            empty.len(),
            2,
            "the one image is both the cell and the viewer"
        );
        assert!(empty.iter().all(|request| request.index == 0));
        assert!(detail_plan(&[], "", 0).is_empty(), "nothing to decode");
    }

    fn skeleton(count: usize) -> Rc<VecModel<ViewerPage>> {
        let pages: Vec<ViewerPage> = (0..count)
            .map(|_| ViewerPage {
                source: slint::Image::default(),
                thumb: slint::Image::default(),
                width: 900,
                height: 1200,
                loaded: false,
                failed: false,
            })
            .collect();
        Rc::new(VecModel::from(pages))
    }

    #[test]
    fn a_decode_from_an_older_epoch_never_paints() {
        let model = skeleton(3);
        let request = DecodeRequest {
            index: 1,
            url: "https://i.pximg.net/img-master/p1_p0_master1200.jpg".into(),
            size: DecodeSize::Viewer,
        };

        // The work changed while this decode was in flight.
        assert!(!adopt_decode(
            &model,
            &request,
            Some(slint::Image::default()),
            false
        ));
        let untouched = model.row_data(1).expect("row");
        assert!(!untouched.loaded, "a stale decode paints nothing");

        // The same result, adopted in the epoch it belongs to.
        assert!(adopt_decode(
            &model,
            &request,
            Some(slint::Image::default()),
            true
        ));
        assert!(model.row_data(1).expect("row").loaded);
        assert!(!model.row_data(0).expect("row").loaded, "only its own row");
    }

    #[test]
    fn a_thumbnail_decode_never_marks_the_viewer_loaded() {
        let model = skeleton(2);
        let request = DecodeRequest {
            index: 0,
            url: "https://i.pximg.net/c/540x540/p0_master1200.jpg".into(),
            size: DecodeSize::Thumb,
        };
        assert!(adopt_decode(
            &model,
            &request,
            Some(slint::Image::default()),
            true
        ));
        let row = model.row_data(0).expect("row");
        assert!(!row.loaded, "a 48 px cell is not the viewer's source");
        assert!(!row.failed);
    }

    #[test]
    fn a_viewer_failure_is_reported_as_failed_even_with_a_thumbnail() {
        let model = skeleton(1);
        let request = DecodeRequest {
            index: 0,
            url: "https://i.pximg.net/img-master/p0_p0_master1200.jpg".into(),
            size: DecodeSize::Viewer,
        };
        assert!(adopt_decode(&model, &request, None, true));
        let row = model.row_data(0).expect("row");
        assert!(row.failed);
        assert!(!row.loaded);
    }

    /// U29: the clamp, and the rule that an out-of-range step changes nothing.
    #[test]
    fn a_page_step_never_leaves_the_work() {
        let table = [
            (0, -1, 5, None),
            (0, 1, 5, Some(1)),
            (2, 1, 5, Some(3)),
            (4, 1, 5, None),
            (4, -1, 5, Some(3)),
            (3, 9, 5, Some(4)),
            (1, -9, 5, Some(0)),
            (0, 0, 5, None),
            (0, 1, 1, None),
            (0, 1, 0, None),
        ];
        for (current, delta, count, expected) in table {
            assert_eq!(
                stepped_page(current, delta, count),
                expected,
                "from {current} by {delta} of {count}"
            );
        }
    }

    /// F4: the glyph decides on this number, so it has to round-trip.
    #[test]
    fn the_restriction_code_matches_the_menu() {
        assert_eq!(restrict_code(BookmarkRestrict::Public), 0);
        assert_eq!(restrict_code(BookmarkRestrict::Private), 1);
        assert_eq!(restrict_from_code(0), BookmarkRestrict::Public);
        assert_eq!(restrict_from_code(1), BookmarkRestrict::Private);
        // The menu can only send 0 or 1; anything else is public, never a
        // half-drawn three-state menu.
        assert_eq!(restrict_from_code(7), BookmarkRestrict::Public);
    }
}
