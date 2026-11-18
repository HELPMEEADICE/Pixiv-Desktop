use super::*;

// The one busy-slot rule every surface uses (see `detail::toggle_allowed`).
use super::detail::toggle_allowed;

/// A user page that is currently open.
pub(crate) struct ProfileState {
    pub(crate) user: Id,
    pub(crate) profile: UserProfile,
    /// Every work id, newest first.
    pub(crate) ids: Vec<Id>,
    /// How many ids have been requested and answered so far.
    pub(crate) loaded: usize,
    pub(crate) works: Vec<Illust>,
    /// Why the id list could not be read, when it could not.
    ///
    /// An empty `ids` has two very different meanings and the page used to collapse
    /// them: "this account has published nothing" and "the request for its works
    /// failed" both rendered as 这个用户还没有公开作品。 (plan §2.3 R18). The message
    /// is kept here so `publish_profile` can say which one it is.
    pub(crate) ids_error: Option<String>,
}

impl App {
    // ------------------------------------------------------------- profile

    /// Load the user page named by the current route.
    pub(crate) fn load_profile(shared: &Shared) {
        let (route, target, epoch, client, rt) = {
            let mut app = shared.borrow_mut();
            let route = app.route;
            let target = route.user_id().map(Id).or(app.session.user_id);
            // One bump per load. This incremented twice, which was harmless for the
            // epoch comparison and made the counter describe nothing (plan §2.3 R20).
            app.epoch += 1;
            let epoch = app.epoch;
            app.profile = None;
            app.feed_owner = None;
            app.items.clear();
            app.columns.clear();
            app.index.clear();
            app.loading_more = false;
            app.reached_end = false;
            {
                let ui = app.ui();
                ui.set_grid_scroll_y(0.0);
                ui.set_grid_state(BAND_LOADING);
                ui.set_grid_columns(ModelRc::default());
                ui.set_grid_loading_more(false);
                ui.set_grid_count_label("".into());
                ui.set_profile_state(BAND_LOADING);
                ui.set_profile_message("".into());
                ui.set_profile_count_label("".into());
                ui.set_profile_followed(false);
                ui.set_profile_avatar(slint::Image::default());
                ui.set_profile_avatar_state(BAND_LOADING);
                ui.set_profile_background(slint::Image::default());
                ui.set_profile_stats(ModelRc::default());
                ui.set_profile_name("".into());
                ui.set_profile_account("".into());
                ui.set_profile_uid(target.map(|id| id.to_string()).unwrap_or_default().into());
                ui.set_profile_comment("".into());
                ui.set_profile_webpage("".into());
                ui.set_profile_premium(false);
                ui.set_profile_self(false);
            }
            app.remember_scroll(route, 0.0);
            app.refresh_chrome();
            (route, target, epoch, app.client.clone(), app.tasks.clone())
        };

        let Some(target) = target else {
            let mut app = shared.borrow_mut();
            app.fail_profile("没有可显示的用户：链接里没有用户 ID，也没有登录账号");
            return;
        };

        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.fail_profile("没有可用的网络客户端");
            return;
        };

        rt.spawn(async move {
            // F3: one document, one fetch. `profile_bundle` reads `/profile/all`
            // once and derives both halves of the answer from it - the counts and the
            // id list - because the two calls this replaced asked for the same 94 KB
            // id map twice through the rate limiter, one for each half. The follower
            // figure is a third request and only for the signed-in account, which is
            // the only session Pixiv lets read a follower list.
            let bundle = client.profile_bundle(target).await;
            let (profile, ids) = match bundle {
                Ok(pair) => pair,
                Err(error) => {
                    let message = describe_session_error(&error);
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.epoch != epoch || app.route != route {
                            return;
                        }
                        app.fail_profile(&message);
                    });
                    return;
                }
            };
            let first: Vec<Id> = ids.iter().take(PROFILE_PAGE).copied().collect();
            // Nothing to ask for when the account published nothing: an empty request
            // would answer "no works" and overwrite the reason with a second lie.
            // A failed work list is a failure too: `unwrap_or_default` turned it into
            // "no works", which is a statement about the artist rather than about the
            // request (T2).
            let (works, works_error) = if first.is_empty() {
                (Vec::new(), None)
            } else {
                match client.user_works(target, &first).await {
                    Ok(works) => (works, None),
                    Err(error) => (Vec::new(), Some(describe_session_error(&error))),
                }
            };

            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                if app.epoch != epoch || app.route != route {
                    return;
                }
                let mut avatar_urls: Vec<String> = Vec::new();
                if let Some(url) = profile.avatar.clone() {
                    avatar_urls.push(url);
                }
                avatar_urls.extend(
                    works
                        .iter()
                        .filter_map(|illust| illust.author.avatar.clone()),
                );
                let state = ProfileState {
                    user: target,
                    profile,
                    ids,
                    loaded: first.len(),
                    works: works.clone(),
                    // The bundle is one document, so there is no longer a case where
                    // the id list failed and the header did not: one request failing
                    // fails the whole page (`fail_profile` above). The field stays
                    // because "the header is here and the works are not" is still a
                    // state the page draws (T1/T2), and it is still set below.
                    ids_error: None,
                };
                app.profile = Some(state);
                app.feed_owner = Some(route);
                app.publish_profile();
                if let Some(message) = works_error.or_else(|| {
                    app.profile
                        .as_ref()
                        .and_then(|state| state.ids_error.clone())
                }) {
                    // The header loaded, the works did not: say so on both bands
                    // instead of leaving the grid on a skeleton with no way back.
                    app.fail_profile_works(&message);
                } else {
                    app.render_feed(works, false);
                }
                app.publish_profile();
                app.spawn_avatars(avatar_urls);
                app.spawn_profile_background();
                app.refresh_chrome();
            });
        });
    }
}

impl App {
    pub(crate) fn spawn_profile_background(&mut self) {
        let Some(url) = self
            .profile
            .as_ref()
            .and_then(|state| state.profile.background.clone())
        else {
            return;
        };
        if self.avatars.contains_key(&url) {
            if let Some(image) = self.avatars.get(&url).cloned() {
                self.ui().set_profile_background(image);
            }
            return;
        }
        if !self.start_avatar(&url) {
            return;
        }
        let cache = self.cache.clone();
        let rt = self.tasks.clone();
        let scale = self.ui().window().scale_factor().max(1.0);
        // Not `target_width`: its clamp is the *card* ceiling, and this image is
        // drawn across the whole header row (plan §2.2 U8).
        let target =
            crate::images::target_width_up_to(1200.0, scale, crate::images::BANNER_MAX_WIDTH);
        rt.spawn(async move {
            let raw = fetch_sized(&cache, &url, target).await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.avatar_loading.remove(&url);
                let Some(raw) = raw else {
                    // The banner has no state property of its own - it is an `image`
                    // and nothing else - so the log line is the whole record, and the
                    // header keeps the initials it drew (T10).
                    tracing::debug!(url = %url, "profile background fetch failed");
                    return;
                };
                let image = raw.to_slint();
                // Through the same eviction path as a face, which also protects
                // this URL: nothing but this function ever requests it (plan §2.3 R7).
                app.remember_avatar(url.clone(), image.clone());
                app.ui().set_profile_background(image);
            });
        });
    }
}

impl App {
    /// A user page that could not be loaded at all.
    ///
    /// Both bands, not just the header: the works grid used to stay on
    /// `BAND_LOADING`, which is a skeleton with no retry and no explanation (plan
    /// §2.3 R20). The header band draws its own retry at state 3
    /// (`ui/pages/profile.slint:306-317`) and Ctrl+R reaches the same reload through
    /// `grid-retry`, so the grid takes no action label of its own: the grid-action
    /// slot navigates to Settings, and that is not a retry.
    pub(crate) fn fail_profile(&mut self, message: &str) {
        self.fail_profile_works(message);
        let ui = self.ui();
        ui.set_profile_state(BAND_ERROR);
        ui.set_profile_message(message.into());
    }

    /// The works half of the same failure, in the page's own words.
    pub(crate) fn fail_profile_works(&mut self, message: &str) {
        let ui = self.ui();
        ui.set_grid_state(BAND_ERROR);
        ui.set_grid_state_title("无法载入用户主页".into());
        ui.set_grid_message(message.into());
        ui.set_grid_action_label("".into());
        ui.set_grid_columns(ModelRc::default());
        self.columns.clear();
        self.index.clear();
    }
}

impl App {
    pub(crate) fn publish_profile(&self) {
        let ui = self.ui();
        let Some(state) = self.profile.as_ref() else {
            return;
        };
        let profile = &state.profile;
        ui.set_profile_name(profile.name.clone().into());
        ui.set_profile_account(profile.account.clone().into());
        ui.set_profile_uid(profile.id.to_string().into());
        ui.set_profile_comment(profile.comment.clone().into());
        ui.set_profile_webpage(profile.webpage.clone().unwrap_or_default().into());
        ui.set_profile_premium(profile.is_premium);
        ui.set_profile_followed(profile.is_followed);
        ui.set_profile_self(self.session.user_id == Some(profile.id));

        let avatar_url = profile.avatar.clone().unwrap_or_default();
        let (image, avatar_state) = match self.avatars.get(&avatar_url) {
            Some(image) => (image.clone(), BAND_OK),
            None if avatar_url.is_empty() => (slint::Image::default(), BAND_EMPTY),
            None => (slint::Image::default(), BAND_LOADING),
        };
        ui.set_profile_avatar(image);
        ui.set_profile_avatar_state(avatar_state);

        ui.set_profile_stats(ModelRc::from(Rc::new(VecModel::from(profile_stats(
            profile,
        )))));

        let total = state.ids.len();
        ui.set_profile_count_label(if total == 0 {
            "".into()
        } else {
            format!("已显示 {} / {} 件作品", state.works.len().min(total), total).into()
        });
        ui.set_profile_state(if state.works.is_empty() {
            self.profile_empty_state()
        } else {
            BAND_OK
        });
        if state.works.is_empty() {
            // Three cases, not two: a failed request for the id list must not be
            // dressed up as a statement about the account (plan §2.3 R18).
            ui.set_profile_message(
                match (state.ids_error.as_deref(), total) {
                    (Some(message), _) => message.to_string(),
                    (None, 0) => "这个用户还没有公开作品。".to_string(),
                    (None, _) => "作品载入失败，点击重试。".to_string(),
                }
                .into(),
            );
        }
    }
}

/// The statistics row of the profile card.
///
/// Figures the server actually sent, and a dash for the one it did not: a count of
/// zero is a claim, and 关注者 is not even readable for anybody but the signed-in
/// account - Pixiv answers a follower list only for the session that owns it, so
/// another artist's page has no number to print and must not print 0 (F3).
///
/// A function of the profile rather than of the window, because the two answers it
/// has to keep apart - "the server said 0" and "the server would not say" - are the
/// whole point, and a test can hold both without a window.
pub(crate) fn profile_stats(profile: &UserProfile) -> Vec<ProfileStatItem> {
    let mut stats = vec![
        ProfileStatItem {
            label: "插画".into(),
            value: profile.illust_count.to_string().into(),
        },
        ProfileStatItem {
            label: "漫画".into(),
            value: profile.manga_count.to_string().into(),
        },
        ProfileStatItem {
            label: "关注".into(),
            value: profile.followees.to_string().into(),
        },
        ProfileStatItem {
            label: "关注者".into(),
            value: profile
                .followers
                .map(|count| count.to_string())
                .unwrap_or_else(|| "—".to_string())
                .into(),
        },
    ];
    if !profile
        .webpage
        .clone()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        stats.push(ProfileStatItem {
            label: "主页".into(),
            value: profile.webpage.clone().unwrap_or_default().into(),
        });
    }
    stats
}

impl App {
    pub(crate) fn profile_empty_state(&self) -> i32 {
        match self.profile.as_ref() {
            // An empty list that came from a failed request is an error, not an
            // empty account: the retry affordance has to be there (plan §2.3 R18).
            Some(state) if state.ids_error.is_some() => BAND_ERROR,
            Some(state) if state.ids.is_empty() => BAND_EMPTY,
            Some(_) => BAND_ERROR,
            None => BAND_LOADING,
        }
    }
}

impl App {
    /// Append the next slice of the artist's works.
    pub(crate) fn load_profile_more(shared: &Shared) {
        let (target, next, epoch, client, rt) = {
            let mut app = shared.borrow_mut();
            if app.loading_more || app.reached_end {
                return;
            }
            let (target, next) = {
                let Some(state) = app.profile.as_ref() else {
                    return;
                };
                if state.loaded >= state.ids.len() {
                    app.reached_end = true;
                    return;
                }
                let next: Vec<Id> = state
                    .ids
                    .iter()
                    .skip(state.loaded)
                    .take(PROFILE_PAGE)
                    .copied()
                    .collect();
                (state.user, next)
            };
            app.loading_more = true;
            app.ui().set_grid_loading_more(true);
            (
                target,
                next,
                app.epoch,
                app.client.clone(),
                app.tasks.clone(),
            )
        };
        let Some(client) = client else {
            let mut app = shared.borrow_mut();
            app.loading_more = false;
            app.ui().set_grid_loading_more(false);
            app.reached_end = true;
            return;
        };
        rt.spawn(async move {
            let requested = next.len();
            let outcome = client
                .user_works(target, &next)
                .await
                .map_err(|error| error.to_string());
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.loading_more = false;
                app.ui().set_grid_loading_more(false);
                if app.epoch != epoch || app.page != Destination::UserProfile {
                    return;
                }
                let urls: Vec<String> = outcome
                    .as_ref()
                    .map(|works| {
                        works
                            .iter()
                            .filter_map(|illust| illust.author.avatar.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let applied = {
                    let Some(state) = app.profile.as_mut() else {
                        return;
                    };
                    apply_profile_more(state, requested, outcome)
                };
                if applied.exhausted {
                    app.reached_end = true;
                }
                app.publish_profile();
                match applied.error {
                    Some(message) => app.notify(3, format!("加载更多失败：{message}")),
                    // Only the rows this page added, appended: `self.items` already
                    // holds everything above them, and handing over the whole list
                    // with append = false rebuilt every column (plan §2.3 R10).
                    None => app.render_feed(applied.added, true),
                }
                app.spawn_avatars(urls);
            });
        });
    }
}

/// What one "load more" attempt did to the page state.
pub(crate) struct MoreApplied {
    /// The rows this attempt actually added, in feed order.
    pub(crate) added: Vec<Illust>,
    /// True when the end of the id list has been reached.
    pub(crate) exhausted: bool,
    /// The server's message, when the page did not come back.
    pub(crate) error: Option<String>,
}

/// Apply one "load more" answer to a user page.
///
/// The cursor only moves on success, and only by the slice that was *answered*.
/// `state.loaded += requested` used to run on both branches, so one failed page
/// marked that slice of the artist's works as fetched for the rest of the session:
/// the page silently under-reported and the missing rows could never be requested
/// again (plan §2.3 R19). A successful page consumes every id it asked about - a
/// work the server returns nothing for (private, deleted) is answered too, and
/// re-requesting that slice would repeat the same page forever - while a failed
/// one consumes nothing, so the next attempt asks for exactly the same slice.
pub(crate) fn apply_profile_more(
    state: &mut ProfileState,
    requested: usize,
    outcome: Result<Vec<Illust>, String>,
) -> MoreApplied {
    match outcome {
        Ok(works) => {
            let before = state.works.len();
            pixiv_types::domain::append_unique(&mut state.works, works);
            state.loaded = (state.loaded + requested).min(state.ids.len());
            MoreApplied {
                added: state.works[before..].to_vec(),
                exhausted: state.loaded >= state.ids.len(),
                error: None,
            }
        }
        Err(message) => MoreApplied {
            added: Vec::new(),
            exhausted: false,
            error: Some(message),
        },
    }
}

/// Write one follow reply onto the user page it belongs to.
///
/// Returns false, and writes nothing, when the page on screen is another account.
/// `publish_profile` republishes `profile-followed` from whatever `ProfileState` is
/// current, so without the check A's reply flipped B's button (B2): a reply is an
/// answer about one account, and a page showing another account is not its home.
/// The key is the account rather than the page's epoch, because the answer stays
/// true for that account even when the page was reloaded while it was in flight.
pub(crate) fn adopt_profile_follow(
    state: Option<&mut ProfileState>,
    asked: Id,
    confirmed: bool,
) -> bool {
    let Some(state) = state else {
        return false;
    };
    // The same rule the illustration page uses (B1/B4/B5), keyed by the account.
    if !reply_lands(
        ReplySurface::Profile(state.user.get()),
        ReplySurface::Profile(asked.get()),
    ) {
        return false;
    }
    state.profile.is_followed = confirmed;
    true
}

impl App {
    /// Follow or unfollow the user on the open profile page.
    ///
    /// Two guards from one invariant (B2): the reply lands on the account that asked
    /// for it, and the page takes one reply at a time - `profile-busy` was written by
    /// every click and read by nothing, so two clicks raced and the later reply won
    /// invisibly.
    pub(crate) fn toggle_profile_follow(&mut self) {
        let Some(state) = self.profile.as_ref() else {
            return;
        };
        let target = state.user;
        if !self.session.is_authenticated() {
            self.notify(2, "需要登录后才能关注");
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        {
            let ui = self.ui();
            if !toggle_allowed(ui.get_profile_busy()) {
                return;
            }
            ui.set_profile_busy(true);
        }
        let rt = self.tasks.clone();
        rt.spawn(async move {
            let outcome = async {
                let current = client.follow_state(target).await?;
                let wanted = !current;
                client.follow(target, wanted).await?;
                let confirmed = client.follow_state(target).await?;
                Ok::<_, pixiv_types::PixivError>((wanted, confirmed))
            }
            .await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                // The slot is free again whatever the answer was.
                app.ui().set_profile_busy(false);
                match outcome {
                    Ok((wanted, confirmed)) => {
                        // The verdict, onto the page that asked for it (B2).
                        if adopt_profile_follow(app.profile.as_mut(), target, confirmed) {
                            app.publish_profile();
                        }
                        if confirmed == wanted {
                            app.notify(
                                1,
                                if confirmed {
                                    "已关注"
                                } else {
                                    "已取消关注"
                                },
                            );
                        } else {
                            app.notify(3, "关注状态未改变：服务器报告的状态与请求不一致");
                        }
                    }
                    Err(error) => app.notify(3, format!("关注失败：{error}")),
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B2 · A's follow reply must not flip B's button.
    ///
    /// The reply used to be written into whatever `ProfileState` was current and
    /// then republished, so a slow answer for account 7 landed on account 9's page.
    /// The plant that makes this red is the identity check removed from
    /// [`adopt_profile_follow`] - the shape the code had.
    #[test]
    fn b2_a_follow_reply_lands_only_on_the_account_that_asked() {
        // The page that asked: the verdict is written, and the republish follows.
        let mut page = page_state();
        page.profile.is_followed = false;
        assert!(adopt_profile_follow(Some(&mut page), Id(7), true));
        assert!(page.profile.is_followed, "the button follows the server");

        // Another account's page is on screen when the reply lands: nothing is
        // written, so the button keeps what the server last said about *it*.
        let mut other = page_state();
        other.user = Id(9);
        other.profile.is_followed = false;
        assert!(
            !adopt_profile_follow(Some(&mut other), Id(7), true),
            "account 7's reply must not reach account 9's page"
        );
        assert!(
            !other.profile.is_followed,
            "the visible button still says what account 9 told us"
        );

        // No page at all draws nothing either.
        assert!(!adopt_profile_follow(None, Id(7), true));

        // The other half of the same invariant: one reply at a time, so the second
        // click of a double click is dropped rather than raced (B2's busy slot).
        assert!(toggle_allowed(false), "the first click goes through");
        assert!(
            !toggle_allowed(true),
            "the second is dropped while one reply is in flight"
        );
    }

    fn work(id: u64) -> Illust {
        history_illust(&pixiv_store::HistoryEntry {
            illust_id: id.to_string(),
            title: format!("w{id}"),
            author: "a".into(),
            thumb_url: format!("https://i.pximg.net/{id}_p0_master1200.jpg"),
            viewed_at: 0,
        })
    }

    /// 48 ids, the first 24 already shown.
    fn page_state() -> ProfileState {
        ProfileState {
            user: Id(7),
            profile: UserProfile::default(),
            ids: (1..=48).map(Id).collect(),
            loaded: 24,
            works: (1..=24).map(work).collect(),
            ids_error: None,
        }
    }

    /// F3: the follower figure is a number only where the server sent one, and a
    /// dash everywhere else - another artist's page, and a signed-in session that
    /// was not allowed to read the list.
    #[test]
    fn the_follower_figure_is_a_dash_where_the_server_would_not_say() {
        let signed_in = UserProfile {
            followers: Some(12_345),
            ..UserProfile::default()
        };
        let row = profile_stats(&signed_in)
            .into_iter()
            .find(|stat| stat.label.as_str() == "关注者")
            .expect("the row exists");
        assert_eq!(row.value.as_str(), "12345");

        // Somebody else's page: the list is not readable, so there is no number.
        let row = profile_stats(&UserProfile::default())
            .into_iter()
            .find(|stat| stat.label.as_str() == "关注者")
            .expect("the row is drawn either way");
        assert_eq!(row.value.as_str(), "—", "a dash, never a 0");

        // And a figure the server *did* send as zero is still a zero, not a dash.
        let zero = UserProfile {
            followers: Some(0),
            ..UserProfile::default()
        };
        let row = profile_stats(&zero)
            .into_iter()
            .find(|stat| stat.label.as_str() == "关注者")
            .expect("the row exists");
        assert_eq!(row.value.as_str(), "0");
    }

    /// The cursor must not move when the page never arrived.
    ///
    /// `state.loaded += requested` ran on both branches, so one failed "load more"
    /// marked that slice of the artist's works as already fetched and the page
    /// under-reported them for the rest of the session (plan §2.3 R19).
    #[test]
    fn a_failed_load_more_does_not_move_the_cursor() {
        let mut state = page_state();
        let applied = apply_profile_more(&mut state, 24, Err("network".to_string()));

        assert_eq!(state.loaded, 24, "the same slice is asked for again");
        assert_eq!(state.works.len(), 24, "nothing was appended");
        assert!(applied.added.is_empty());
        assert!(!applied.exhausted);
        assert_eq!(applied.error.as_deref(), Some("network"));

        // And the retry does ask for exactly the slice that failed.
        let next: Vec<Id> = state
            .ids
            .iter()
            .skip(state.loaded)
            .take(24)
            .copied()
            .collect();
        assert_eq!(next.first().map(|id| id.get()), Some(25));
    }

    /// A page that arrived advances the cursor by the slice it answered, and hands
    /// back only the rows it added - the shape `render_feed(.., true)` needs.
    #[test]
    fn a_page_that_arrived_advances_the_cursor_and_returns_only_new_rows() {
        let mut state = page_state();
        // 23 new works, and one the page already showed.
        let mut page: Vec<Illust> = (25..=47).map(work).collect();
        page.push(work(3));
        let applied = apply_profile_more(&mut state, 24, Ok(page));

        assert_eq!(state.loaded, 48, "the answered slice is consumed");
        assert_eq!(state.works.len(), 47, "the duplicate is not appended twice");
        assert_eq!(applied.added.len(), 23, "only the new rows are handed over");
        assert_eq!(
            applied.added.first().map(|illust| illust.id.get()),
            Some(25)
        );
        assert!(applied.exhausted, "the id list is now fully shown");
        assert!(applied.error.is_none());
    }
}
