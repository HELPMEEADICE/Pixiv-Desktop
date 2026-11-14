use super::*;

/// A user page that is currently open.
pub(crate) struct ProfileState {
    pub(crate) user: Id,
    pub(crate) profile: UserProfile,
    /// Every work id, newest first.
    pub(crate) ids: Vec<Id>,
    /// How many ids have been requested so far.
    pub(crate) loaded: usize,
    pub(crate) works: Vec<Illust>,
}

impl App {
    // ------------------------------------------------------------- profile

    /// Load the user page named by the current route.
    pub(crate) fn load_profile(shared: &Shared) {
        let (route, target, epoch, client, rt) = {
            let mut app = shared.borrow_mut();
            let route = app.route;
            let target = route.user_id().map(Id).or(app.session.user_id);
            app.epoch += 1;
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
            let app = shared.borrow_mut();
            app.ui().set_profile_state(BAND_ERROR);
            app.ui()
                .set_profile_message("没有可显示的用户：链接里没有用户 ID，也没有登录账号".into());
            return;
        };

        let Some(client) = client else {
            let app = shared.borrow_mut();
            app.ui().set_profile_state(BAND_ERROR);
            app.ui().set_profile_message("没有可用的网络客户端".into());
            return;
        };

        rt.spawn(async move {
            let (profile, ids) =
                tokio::join!(client.user_profile(target), client.user_illust_ids(target));
            let profile = match profile {
                Ok(profile) => profile,
                Err(error) => {
                    let message = error.to_string();
                    on_ui(move |shared| {
                        let Ok(app) = shared.try_borrow_mut() else {
                            return;
                        };
                        if app.epoch != epoch || app.route != route {
                            return;
                        }
                        app.ui().set_profile_state(BAND_ERROR);
                        app.ui().set_profile_message(message.clone().into());
                        app.ui().set_grid_state(BAND_ERROR);
                        app.ui().set_grid_state_title("无法载入用户主页".into());
                        app.ui().set_grid_message(message.into());
                    });
                    return;
                }
            };
            let ids = ids.unwrap_or_default();
            let first: Vec<Id> = ids.iter().take(PROFILE_PAGE).copied().collect();
            let works = client.user_works(target, &first).await.unwrap_or_default();

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
                };
                app.profile = Some(state);
                app.feed_owner = Some(route);
                app.publish_profile();
                app.render_feed(works, false);
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
        let target = crate::images::target_width(1200.0, scale);
        rt.spawn(async move {
            let raw = fetch_sized(&cache, &url, target).await;
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.avatar_loading.remove(&url);
                let Some(raw) = raw else {
                    return;
                };
                let image = raw.to_slint();
                app.avatars.insert(url.clone(), image.clone());
                app.ui().set_profile_background(image);
            });
        });
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
            ui.set_profile_message(
                if total == 0 {
                    "这个用户还没有公开作品。"
                } else {
                    "作品载入失败，点击重试。"
                }
                .into(),
            );
        }
    }
}

impl App {
    pub(crate) fn profile_empty_state(&self) -> i32 {
        match self.profile.as_ref() {
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
            let works = client.user_works(target, &next).await.unwrap_or_default();
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.loading_more = false;
                app.ui().set_grid_loading_more(false);
                if app.epoch != epoch || app.page != Destination::UserProfile {
                    return;
                }
                let urls: Vec<String> = works
                    .iter()
                    .filter_map(|illust| illust.author.avatar.clone())
                    .collect();
                let requested = next.len();
                let (exhausted, all_works) = {
                    let Some(state) = app.profile.as_mut() else {
                        return;
                    };
                    state.loaded += requested;
                    pixiv_types::domain::append_unique(&mut state.works, works);
                    (state.loaded >= state.ids.len(), state.works.clone())
                };
                if exhausted {
                    app.reached_end = true;
                }
                app.publish_profile();
                app.render_feed(all_works, false);
                app.spawn_avatars(urls);
            });
        });
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
