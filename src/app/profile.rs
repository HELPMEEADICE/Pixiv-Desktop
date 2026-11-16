use super::*;

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
            (route, target, epoch, app.client.clone(), app.rt.clone())
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
            let (profile, ids) =
                tokio::join!(client.user_profile(target), client.user_illust_ids(target));
            let profile = match profile {
                Ok(profile) => profile,
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
            // A failed id list is not an empty account. `unwrap_or_default` turned
            // the error into "no works", which the page then rendered as a statement
            // about the artist (plan §2.3 R18). Keep the Result and carry the real
            // message to the page.
            let (ids, ids_error) = match ids {
                Ok(ids) => (ids, None),
                Err(error) => (Vec::new(), Some(describe_session_error(&error))),
            };
            let first: Vec<Id> = ids.iter().take(PROFILE_PAGE).copied().collect();
            // Nothing to ask for when the id list itself failed: an empty request
            // would answer "no works" and overwrite the reason with a second lie.
            // A failed work list is a failure too: `unwrap_or_default` turned it into
            // "no works", which is a statement about the artist rather than about the
            // request (T2).
            let (works, works_error) = if ids_error.is_some() {
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
                    ids_error,
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
        let rt = self.rt.clone();
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

        // Figures the server actually sent. A count it did not send is shown as
        // a dash rather than as a zero, because zero is a claim.
        let mut stats: Vec<ProfileStatItem> = vec![
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
                label: "粉丝".into(),
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
        ui.set_profile_stats(ModelRc::from(Rc::new(VecModel::from(stats))));

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
            (target, next, app.epoch, app.client.clone(), app.rt.clone())
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

impl App {
    /// Follow or unfollow the user on the open profile page.
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
        self.ui().set_profile_busy(true);
        let rt = self.rt.clone();
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
                app.ui().set_profile_busy(false);
                match outcome {
                    Ok((wanted, confirmed)) => {
                        if let Some(state) = app.profile.as_mut() {
                            state.profile.is_followed = confirmed;
                        }
                        app.publish_profile();
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
