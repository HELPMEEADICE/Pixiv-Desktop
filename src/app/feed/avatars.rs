use std::cell::RefCell;
use std::collections::VecDeque;

use super::*;

thread_local! {
    /// Insertion order for [`App::avatars`], so eviction can drop the oldest
    /// faces first (plan §2.3 R7). The map itself is declared in `app/mod.rs` and
    /// stays a plain `HashMap`; the order lives beside the only code that writes
    /// it, the way `mod.rs` keeps the single `App` handle in a `thread_local!`.
    static AVATAR_ORDER: RefCell<VecDeque<String>> = const { RefCell::new(VecDeque::new()) };
}

/// Store one decoded face, evicting the oldest faces and nothing else.
///
/// The map used to be `clear()`ed the moment it passed `AVATARS_KEEP`, which took
/// the profile banner with it: the banner is stored here too (`profile.rs:172`)
/// and `spawn_profile_background` is its only requester, so an evicted banner
/// stayed blank for the life of the page. Rows already on screen hold their own
/// copy of an `Image`, so dropping a *face* never blanks a visible avatar - the
/// next render re-requests it and gets it from the disk/memory tier. That is true
/// of a face and false of the banner, which is why `protect` exists.
pub(crate) fn insert_avatar(
    avatars: &mut HashMap<String, slint::Image>,
    order: &mut VecDeque<String>,
    url: String,
    image: slint::Image,
    keep: usize,
    protect: Option<&str>,
) {
    avatars.insert(url.clone(), image);
    if let Some(seen) = order.iter().position(|known| *known == url) {
        order.remove(seen);
    }
    order.push_back(url);
    // A cap of zero entries is not a queue, it is a licence to loop forever.
    let keep = keep.max(1);
    while avatars.len() > keep {
        let Some(oldest) = order.pop_front() else {
            break;
        };
        if Some(oldest.as_str()) == protect {
            // Never dropped, but sent to the back: the one entry that cannot be
            // re-requested must not block the next eviction.
            order.push_back(oldest);
            continue;
        }
        avatars.remove(&oldest);
    }
}

impl App {
    /// One decoded face in, the oldest faces out.
    ///
    /// The banner is protected by URL rather than by a field of its own: the
    /// field would have to be declared in `App` (`app/mod.rs`), which is outside
    /// this round's boundary, and the URL is already known exactly - it is
    /// `profile.background`, and `spawn_profile_background` is its only requester.
    pub(crate) fn remember_avatar(&mut self, url: String, image: slint::Image) {
        let banner = self
            .profile
            .as_ref()
            .and_then(|state| state.profile.background.clone());
        let avatars = &mut self.avatars;
        AVATAR_ORDER.with(|order| {
            let mut order = order.borrow_mut();
            insert_avatar(
                avatars,
                &mut order,
                url,
                image,
                AVATARS_KEEP,
                banner.as_deref(),
            );
        });
    }
}

/// The same one-row patch for a band of cards, which carries the author avatar
/// per row (`IllustItem.author-avatar-url`).
///
/// The home "latest" band used to be refreshed only as a side effect of
/// `publish_home` running for an artist's face; with arrivals patched instead of
/// re-published, the band is patched too rather than left on a placeholder.
pub(crate) fn patch_card_avatar_row(
    model: &ModelRc<IllustItem>,
    works: &[Illust],
    url: &str,
    image: &slint::Image,
) -> usize {
    let mut patched = 0;
    for (row, illust) in works.iter().enumerate() {
        if illust.author.avatar.as_deref() != Some(url) {
            continue;
        }
        let Some(mut item) = model.row_data(row) else {
            continue;
        };
        item.author_avatar = image.clone();
        item.author_avatar_state = BAND_OK;
        model.set_row_data(row, item);
        patched += 1;
    }
    patched
}
/// Point the row that owns `url` at the face that just arrived.
///
/// Returns how many rows were written. The model is patched in place, so its
/// identity and the order of its rows are unchanged - which is the whole
/// difference between this and re-running `publish_home` for one face
/// (plan §2.3 R15). A row whose `avatar-url` does not match belongs to somebody
/// else and is left alone.
pub(crate) fn patch_avatar_row(
    model: &ModelRc<RecommendUserItem>,
    users: &[RecommendUser],
    url: &str,
    image: &slint::Image,
) -> usize {
    let mut patched = 0;
    for (row, user) in users.iter().enumerate() {
        if user.avatar.as_deref() != Some(url) {
            continue;
        }
        let Some(mut item) = model.row_data(row) else {
            continue;
        };
        item.avatar = image.clone();
        item.avatar_state = BAND_OK;
        model.set_row_data(row, item);
        patched += 1;
    }
    patched
}

/// The same one-row patch, for a face that did not arrive.
///
/// A failed fetch used to drop its loading key and return, which left the row on
/// `BAND_LOADING` for the life of the page: a spinner that never stops is a
/// promise the app cannot keep (T10). The row moves to the state the pages draw
/// for "no face" - 2, failed or absent - and keeps the initial it already had.
pub(crate) fn fail_avatar_row(
    model: &ModelRc<RecommendUserItem>,
    users: &[RecommendUser],
    url: &str,
) -> usize {
    let mut patched = 0;
    for (row, user) in users.iter().enumerate() {
        if user.avatar.as_deref() != Some(url) {
            continue;
        }
        let Some(mut item) = model.row_data(row) else {
            continue;
        };
        if item.avatar_state == BAND_EMPTY {
            continue;
        }
        item.avatar_state = BAND_EMPTY;
        model.set_row_data(row, item);
        patched += 1;
    }
    patched
}

/// The card half of the same rule (T10).
pub(crate) fn fail_card_avatar_row(
    model: &ModelRc<IllustItem>,
    works: &[Illust],
    url: &str,
) -> usize {
    let mut patched = 0;
    for (row, illust) in works.iter().enumerate() {
        if illust.author.avatar.as_deref() != Some(url) {
            continue;
        }
        let Some(mut item) = model.row_data(row) else {
            continue;
        };
        if item.author_avatar_state == BAND_EMPTY {
            continue;
        }
        item.author_avatar_state = BAND_EMPTY;
        model.set_row_data(row, item);
        patched += 1;
    }
    patched
}

impl App {
    /// Park every row that was waiting for this face in its failure state.
    ///
    /// Returns how many surfaces were written, which is what the debug line at the
    /// call site reports: a failure that reaches neither a page state nor a log is
    /// exactly the defect T10 names.
    pub(crate) fn fail_avatars(&mut self, url: &str) -> usize {
        let mut patched = 0;
        if self.account_avatar.as_deref() == Some(url) {
            self.ui().set_account_avatar_state(BAND_EMPTY);
            patched += 1;
        }
        if self
            .profile
            .as_ref()
            .and_then(|state| state.profile.avatar.clone())
            .as_deref()
            == Some(url)
        {
            self.ui().set_profile_avatar_state(BAND_EMPTY);
            patched += 1;
        }
        if self.detail_author_avatar_url.as_deref() == Some(url) {
            self.ui().set_detail_author_avatar_state(BAND_EMPTY);
            patched += 1;
        }
        let model = self.ui().get_home_artists();
        patched += fail_avatar_row(&model, &self.home.artists, url);
        let model = self.ui().get_discovery_users();
        patched += fail_avatar_row(&model, &self.discovery.users, url);
        let model = self.ui().get_home_latest();
        patched += fail_card_avatar_row(&model, &self.home.latest, url);
        for column in &self.columns {
            for row in 0..column.row_count() {
                let Some(mut item) = column.row_data(row) else {
                    continue;
                };
                if item.author_avatar_url.as_str() != url || item.author_avatar_state == BAND_EMPTY
                {
                    continue;
                }
                item.author_avatar_state = BAND_EMPTY;
                column.set_row_data(row, item);
                patched += 1;
            }
        }
        patched
    }

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
                        // T10: the row leaves BAND_LOADING instead of waiting for a
                        // face that is not coming, and the reason survives in the log.
                        let parked = app.fail_avatars(&url);
                        tracing::debug!(
                            url = %url,
                            parked,
                            "avatar fetch failed; the rows that wanted it are done loading"
                        );
                        return;
                    };
                    let image = raw.to_slint();
                    app.remember_avatar(url.clone(), image);
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

        // The two user bands are patched, not re-published. Rebuilding them made
        // one arrival re-create every row of the band - a home load with 40 faces
        // rebuilt every row 40 times - because `publish_home` and
        // `publish_discovery_users` each build a new VecModel per call. Only the
        // row that owns this URL is written, the way `apply_cover` does it
        // (plan §2.3 R15). A band that is republished for another reason (a new
        // payload from `load_home` / `load_discovery`) still goes through those
        // functions; no arrival path does any more.
        if let Some(image) = self.avatars.get(url).cloned() {
            let model = self.ui().get_home_artists();
            patch_avatar_row(&model, &self.home.artists, url, &image);
            let model = self.ui().get_discovery_users();
            patch_avatar_row(&model, &self.discovery.users, url, &image);
            // The home "latest" band is a row of cards, and it used to be refreshed
            // only as a side effect of `publish_home` running for an artist's face.
            let model = self.ui().get_home_latest();
            patch_card_avatar_row(&model, &self.home.latest, url, &image);
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
                let found = match client.user_profile(Id(author)).await {
                    Ok(profile) => profile.avatar,
                    Err(error) => {
                        // No face is a row that keeps its initial, which is not worth
                        // a page state - but it is worth a line (G41).
                        tracing::debug!(%error, artist = author, "author avatar lookup failed");
                        None
                    }
                };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn face() -> slint::Image {
        slint::Image::default()
    }

    /// Avatar pressure must not empty the map, and must never take the banner
    /// with it. The old code `clear()`ed at the cap, so the profile header lost
    /// its cover after 256 faces and nothing re-requested it (plan §2.3 R7).
    #[test]
    fn the_avatar_map_drops_the_oldest_face_and_never_the_banner() {
        let mut avatars: HashMap<String, slint::Image> = HashMap::new();
        let mut order: VecDeque<String> = VecDeque::new();
        // The banner is the oldest entry, which is exactly the one wholesale
        // eviction used to throw away first.
        insert_avatar(
            &mut avatars,
            &mut order,
            "banner".into(),
            face(),
            AVATARS_KEEP,
            Some("banner"),
        );
        for index in 0..300 {
            insert_avatar(
                &mut avatars,
                &mut order,
                format!("u{index}"),
                face(),
                AVATARS_KEEP,
                Some("banner"),
            );
            assert!(
                avatars.len() <= AVATARS_KEEP,
                "the map never grows past the cap"
            );
        }
        assert_eq!(avatars.len(), AVATARS_KEEP);
        assert!(
            avatars.contains_key("banner"),
            "the banner has no re-request path, so it is never evicted"
        );
        assert!(avatars.contains_key("u299"), "the newest faces are kept");
        assert!(!avatars.contains_key("u0"), "the oldest faces go first");
        assert_eq!(order.len(), avatars.len(), "the order tracks the map");
    }

    /// The same rule for a band of cards: one row, same model, same order.
    #[test]
    fn an_avatar_arrival_patches_one_card_row() {
        let url = "https://i.pximg.net/face.jpg";
        let mut works: Vec<Illust> = (1..=3).map(|id| work_with_face(id, Some(url))).collect();
        works[1].author.avatar = Some("https://i.pximg.net/other.jpg".to_string());
        let rows: Vec<IllustItem> = works
            .iter()
            .map(|illust| IllustItem {
                id: illust.id.to_string().into(),
                title: "".into(),
                author: "".into(),
                author_id: "".into(),
                cover: slint::Image::default(),
                kind: 0,
                page_count: 1,
                bookmarked: false,
                rank: 0,
                width: 0,
                height: 0,
                state: 0,
                author_avatar: slint::Image::default(),
                author_avatar_url: illust.author.avatar.clone().unwrap_or_default().into(),
                author_avatar_state: BAND_LOADING,
                selected: false,
                selectable: false,
            })
            .collect();
        let model = Rc::new(VecModel::from(rows));
        let published = ModelRc::from(model.clone());

        assert_eq!(patch_card_avatar_row(&published, &works, url, &face()), 2);
        assert_eq!(model.row_count(), 3, "no row is added or dropped");
        assert_eq!(model.row_data(0).unwrap().author_avatar_state, BAND_OK);
        assert_eq!(model.row_data(1).unwrap().author_avatar_state, BAND_LOADING);
        assert_eq!(model.row_data(2).unwrap().author_avatar_state, BAND_OK);
    }

    fn work_with_face(id: u64, avatar: Option<&str>) -> Illust {
        let mut illust = history_illust(&pixiv_store::HistoryEntry {
            illust_id: format!("{id}0"),
            title: format!("w{id}"),
            author: "a".into(),
            thumb_url: "https://i.pximg.net/c.jpg".into(),
            viewed_at: 0,
        });
        illust.author.avatar = avatar.map(str::to_string);
        illust
    }
    fn user(id: u64, avatar: Option<&str>) -> RecommendUser {
        RecommendUser {
            id: Id(id),
            name: format!("u{id}"),
            account: format!("a{id}"),
            comment: String::new(),
            avatar: avatar.map(str::to_string),
            background: None,
            previews: Vec::new(),
            is_followed: false,
            is_premium: false,
        }
    }

    /// One arrival patches one row: the model keeps its identity, its length and
    /// its order, and only the row that owns the URL changes.
    ///
    /// Re-publishing rebuilt the band - a new VecModel for every row on every
    /// arrival, so a home load with 40 faces re-created every row 40 times
    /// (plan §2.3 R15).
    #[test]
    fn an_avatar_arrival_patches_one_row_and_keeps_the_model() {
        let users = vec![
            user(1, Some("https://i.pximg.net/a.jpg")),
            user(2, Some("https://i.pximg.net/b.jpg")),
            user(3, None),
        ];
        let rows: Vec<RecommendUserItem> = users
            .iter()
            .map(|user| RecommendUserItem {
                id: user.id.to_string().into(),
                name: user.name.clone().into(),
                account: user.account.clone().into(),
                comment: "".into(),
                avatar: slint::Image::default(),
                avatar_url: user.avatar.clone().unwrap_or_default().into(),
                preview: slint::Image::default(),
                followed: false,
                premium: false,
                avatar_state: BAND_LOADING,
            })
            .collect();
        let model = Rc::new(VecModel::from(rows));
        let published = ModelRc::from(model.clone());

        assert_eq!(
            patch_avatar_row(&published, &users, "https://i.pximg.net/b.jpg", &face()),
            1
        );
        // Same model, same row count, same order.
        assert_eq!(model.row_count(), 3);
        assert_eq!(model.row_data(1).unwrap().id.to_string(), "2");
        // Exactly one row changed: the second one now holds the face and reports OK.
        assert_eq!(model.row_data(1).unwrap().avatar_state, BAND_OK);
        assert_eq!(model.row_data(0).unwrap().avatar_state, BAND_LOADING);
        assert_eq!(model.row_data(2).unwrap().avatar_state, BAND_LOADING);

        // A URL no row owns writes nothing at all.
        assert_eq!(
            patch_avatar_row(&published, &users, "https://i.pximg.net/none.jpg", &face()),
            0
        );
    }
    /// Re-inserting a URL that is already held must not duplicate its slot in
    /// the order, or one re-decode would keep two entries alive.
    #[test]
    fn a_repeated_url_keeps_one_slot_in_the_order() {
        let mut avatars: HashMap<String, slint::Image> = HashMap::new();
        let mut order: VecDeque<String> = VecDeque::new();
        for _ in 0..3 {
            insert_avatar(&mut avatars, &mut order, "a".into(), face(), 2, None);
        }
        assert_eq!(avatars.len(), 1);
        assert_eq!(order.len(), 1);
    }

    /// T10 - a face that did not arrive parks its row instead of spinning forever.
    ///
    /// The fetch used to drop its loading key and return, leaving the row on
    /// BAND_LOADING for the life of the page. On the old tree there is no failure
    /// state to assert and the row stays at BAND_LOADING: that is the assertion that
    /// fails.
    #[test]
    fn t10_a_failed_avatar_parks_the_row_in_its_failure_state() {
        let url = "https://i.pximg.net/face.jpg";
        let users = vec![
            user(1, Some(url)),
            user(2, Some("https://i.pximg.net/other.jpg")),
        ];
        let rows: Vec<RecommendUserItem> = users
            .iter()
            .map(|user| RecommendUserItem {
                id: user.id.to_string().into(),
                name: user.name.clone().into(),
                account: user.account.clone().into(),
                comment: "".into(),
                avatar: slint::Image::default(),
                avatar_url: user.avatar.clone().unwrap_or_default().into(),
                preview: slint::Image::default(),
                followed: false,
                premium: false,
                avatar_state: BAND_LOADING,
            })
            .collect();
        let model = Rc::new(VecModel::from(rows));
        let published = ModelRc::from(model.clone());

        assert_eq!(fail_avatar_row(&published, &users, url), 1);
        assert_eq!(
            model.row_data(0).unwrap().avatar_state,
            BAND_EMPTY,
            "the row leaves BAND_LOADING when the fetch fails"
        );
        assert_eq!(
            model.row_data(1).unwrap().avatar_state,
            BAND_LOADING,
            "another row's face is not touched"
        );
        // The patch is idempotent: a second failure of the same URL writes nothing.
        assert_eq!(fail_avatar_row(&published, &users, url), 0);

        // The card half of the same rule.
        let works = vec![
            work_with_face(1, Some(url)),
            work_with_face(2, Some("https://i.pximg.net/other.jpg")),
        ];
        let cards: Vec<IllustItem> = works
            .iter()
            .map(|illust| IllustItem {
                id: illust.id.to_string().into(),
                title: "".into(),
                author: "".into(),
                author_id: "".into(),
                cover: slint::Image::default(),
                kind: 0,
                page_count: 1,
                bookmarked: false,
                rank: 0,
                width: 0,
                height: 0,
                state: 0,
                author_avatar: slint::Image::default(),
                author_avatar_url: illust.author.avatar.clone().unwrap_or_default().into(),
                author_avatar_state: BAND_LOADING,
                selected: false,
                selectable: false,
            })
            .collect();
        let cards = Rc::new(VecModel::from(cards));
        let published_cards = ModelRc::from(cards.clone());
        assert_eq!(fail_card_avatar_row(&published_cards, &works, url), 1);
        assert_eq!(
            cards.row_data(0).unwrap().author_avatar_state,
            BAND_EMPTY,
            "the card's author face leaves BAND_LOADING too"
        );
        assert_eq!(cards.row_data(1).unwrap().author_avatar_state, BAND_LOADING);
    }
}
