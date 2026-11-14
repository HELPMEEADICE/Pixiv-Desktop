use super::*;

/// Search-suggestion panel state.
pub(crate) struct SuggestState {
    /// Bumped on every edit and every close. A result whose generation no longer
    /// matches is dropped, which is what stops a slow reply for "lan" from
    /// overwriting the list for "landscape".
    pub(crate) generation: u64,
    pub(crate) query: String,
    pub(crate) visible: bool,
    pub(crate) rows: Vec<Suggestion>,
    pub(crate) active: i32,
    pub(crate) note: String,
    pub(crate) timer: slint::Timer,
    /// Focus-state rows: the server's own recommended tags, plus local history.
    pub(crate) base: Vec<Suggestion>,
}

impl Default for SuggestState {
    fn default() -> Self {
        Self {
            generation: 0,
            query: String::new(),
            visible: false,
            rows: Vec::new(),
            active: -1,
            note: String::new(),
            timer: slint::Timer::default(),
            base: Vec::new(),
        }
    }
}

impl App {
    // ------------------------------------------------------------- suggestions

    pub(crate) fn publish_suggestions(&self) {
        let ui = self.ui();
        let rows: Vec<SuggestionItem> = self
            .suggest
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| SuggestionItem {
                tag: row.tag.clone().into(),
                label: row.display().to_string().into(),
                count: row
                    .illust_count
                    .map(|count| format!("{count} 件"))
                    .unwrap_or_default()
                    .into(),
                local: matches!(
                    row.source,
                    pixiv_types::domain::SuggestionSource::LocalHistory
                ),
                active: index as i32 == self.suggest.active,
            })
            .collect();
        ui.set_suggestions(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_suggest_visible(self.suggest.visible);
        ui.set_suggest_note(self.suggest.note.clone().into());
    }
}

impl App {
    pub(crate) fn suggest_close(&mut self) {
        self.suggest.generation += 1;
        self.suggest.timer.stop();
        self.suggest.visible = false;
        self.suggest.active = -1;
        self.suggest.rows.clear();
        self.suggest.note.clear();
        self.publish_suggestions();
    }
}

impl App {
    /// Rows for an empty box: the server's own recommended tags, then this
    /// machine's search history, which is labelled as local.
    pub(crate) fn suggest_show_base(&mut self) {
        let mut rows = self.suggest.base.clone();
        let history = self.store.search_history(5).unwrap_or_default();
        let local = pixiv_types::domain::history_suggestions(&history, 5);
        let had_history = !local.is_empty();
        rows.extend(local);
        self.suggest.rows = rows;
        self.suggest.active = -1;
        self.suggest.visible = !self.suggest.rows.is_empty();
        self.suggest.note = if self.suggest.base.is_empty() && had_history {
            "只有本机搜索记录，Pixiv 没有返回推荐标签".to_string()
        } else if had_history {
            "上方为 Pixiv 推荐标签，下方为本机搜索记录".to_string()
        } else {
            String::new()
        };
        self.publish_suggestions();
    }
}

impl App {
    /// The user typed. Debounce, then ask Pixiv.
    pub(crate) fn suggest_query(&mut self, shared: &Shared, text: &str) {
        self.suggest.generation += 1;
        self.suggest.query = text.trim().to_string();
        self.suggest.timer.stop();
        if self.suggest.query.is_empty() {
            self.suggest_show_base();
            return;
        }

        // Show the panel immediately with an explicit loading note: an empty
        // dropdown while typing reads as "no results".
        self.suggest.visible = true;
        self.suggest.active = -1;
        self.suggest.note = "正在向 Pixiv 请求联想…".to_string();
        self.suggest.rows.clear();
        self.publish_suggestions();

        let generation = self.suggest.generation;
        let shared = shared.clone();
        // 280ms: long enough that a normal typing run issues one request, short
        // enough that the list feels live.
        self.suggest.timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(280),
            move || {
                let (word, client, rt) = {
                    let app = shared.borrow();
                    (
                        app.suggest.query.clone(),
                        app.client.clone(),
                        app.rt.clone(),
                    )
                };
                if word.is_empty() {
                    return;
                }
                let Some(client) = client else {
                    let mut app = shared.borrow_mut();
                    app.suggest.note = "未登录：联想需要网络客户端".to_string();
                    app.publish_suggestions();
                    return;
                };
                rt.spawn(async move {
                    let rows = client.suggestions(&word, 10).await;
                    on_ui(move |shared| {
                        let Ok(mut app) = shared.try_borrow_mut() else {
                            return;
                        };
                        // A reply for a query the user has already moved past
                        // must not overwrite the newer list.
                        if app.suggest.generation != generation {
                            return;
                        }
                        match rows {
                            Ok(rows) if !rows.is_empty() => {
                                app.suggest.rows = rows;
                                app.suggest.active = -1;
                                app.suggest.visible = true;
                                app.suggest.note = String::new();
                            }
                            Ok(_) => {
                                app.suggest.rows.clear();
                                app.suggest.active = -1;
                                app.suggest.visible = true;
                                app.suggest.note =
                                    "Pixiv 没有返回匹配标签，按回车可直接搜索这个词".to_string();
                            }
                            Err(error) => {
                                // Local history is offered as a clearly separate
                                // aid, never dressed up as a Pixiv suggestion.
                                let history = app.store.search_history(5).unwrap_or_default();
                                app.suggest.rows =
                                    pixiv_types::domain::history_suggestions(&history, 5);
                                app.suggest.active = -1;
                                app.suggest.visible = true;
                                app.suggest.note =
                                    format!("联想服务不可用（{error}），以上为本机搜索记录");
                            }
                        }
                        app.publish_suggestions();
                    });
                });
            },
        );
    }
}

impl App {
    /// Up / Down inside the search field.
    pub(crate) fn suggest_move(&mut self, key: &str) {
        if !self.suggest.visible || self.suggest.rows.is_empty() {
            return;
        }
        let count = self.suggest.rows.len() as i32;
        let down = key == "\u{F701}";
        let next = if down {
            if self.suggest.active + 1 >= count {
                0
            } else {
                self.suggest.active + 1
            }
        } else if self.suggest.active <= 0 {
            count - 1
        } else {
            self.suggest.active - 1
        };
        self.suggest.active = next;
        self.publish_suggestions();
    }
}

impl App {
    /// The chosen row, or the typed text when no row is highlighted.
    pub(crate) fn suggest_resolve(&self, typed: &str) -> String {
        if self.suggest.visible
            && self.suggest.active >= 0
            && let Some(row) = self.suggest.rows.get(self.suggest.active as usize)
        {
            return row.tag.clone();
        }
        typed.to_string()
    }
}
