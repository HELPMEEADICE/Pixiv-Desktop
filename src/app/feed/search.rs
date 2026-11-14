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
    /// The model the panel is bound to: one for the life of the process, patched
    /// in place.
    ///
    /// It has to be the same model every publish, because the U6 auto-scroll is
    /// driven by "this row *became* active" - a row that is created already
    /// active never emits that change, so a rebuilt model would leave the cursor
    /// off screen and the panel's scroll position at zero.
    pub(crate) rows_model: Rc<VecModel<SuggestionItem>>,
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
            rows_model: Rc::new(VecModel::default()),
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
        sync_suggestion_rows(&self.suggest.rows_model, rows);
        // The same model every time: Slint keeps the row elements it is not told
        // to drop, which is what lets a row report that it just became active.
        ui.set_suggestions(ModelRc::from(self.suggest.rows_model.clone()));
        ui.set_suggest_visible(self.suggest.visible);
        ui.set_suggest_note(self.suggest.note.clone().into());
    }
}

/// Patch `model` so it holds `next`, touching only the rows that differ.
///
/// Returns how many rows were written. Fields are compared one by one rather than
/// through PartialEq on a generated Slint struct, for the same reason
/// `sync_download_rows` does it: a field added to the struct cannot silently make
/// every row compare equal and stop the panel from updating.
pub(crate) fn sync_suggestion_rows(
    model: &VecModel<SuggestionItem>,
    next: Vec<SuggestionItem>,
) -> usize {
    let mut touched = 0;
    while model.row_count() > next.len() {
        model.remove(model.row_count() - 1);
        touched += 1;
    }
    for (index, row) in next.into_iter().enumerate() {
        match model.row_data(index) {
            Some(current) if same_suggestion_row(&current, &row) => {}
            Some(_) => {
                model.set_row_data(index, row);
                touched += 1;
            }
            None => {
                model.push(row);
                touched += 1;
            }
        }
    }
    touched
}

fn same_suggestion_row(a: &SuggestionItem, b: &SuggestionItem) -> bool {
    a.tag == b.tag
        && a.label == b.label
        && a.count == b.count
        && a.local == b.local
        && a.active == b.active
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

impl App {
    /// PIXIV_DESKTOP_FIXTURE=suggestion: the panel, with rows in it.
    ///
    /// U6's fix - a ScrollView and a cursor row that scrolls itself into view -
    /// had no visual evidence, because the demo data source produces no
    /// suggestion rows at all (publish_suggestions clears the model on an empty
    /// query). Twelve fixed tags and a ninth-row cursor make the state
    /// photographable without a single network request.
    pub(crate) fn suggest_fixture(&mut self) {
        const TAGS: [&str; 12] = [
            "桜",
            "メカ",
            "和服",
            "厚塗り",
            "女の子",
            "魔法",
            "猫",
            "瞳",
            "空",
            "背景",
            "雨",
            "オリジナル",
        ];
        self.suggest.rows = TAGS
            .into_iter()
            .map(|tag| Suggestion {
                tag: tag.to_string(),
                translated: None,
                illust_count: None,
                popularity: None,
                source: pixiv_types::domain::SuggestionSource::Pixiv,
            })
            .collect();
        self.suggest.visible = true;
        self.suggest.note.clear();
        self.suggest.active = -1;
        self.publish_suggestions();

        // The cursor is set one frame later, on purpose: a row that is created
        // already active never emits the change that the panel turns into a
        // scroll position, so publishing it active straight away would leave the
        // panel unscrolled and prove nothing about U6's auto-scroll.
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(400),
            || {
                on_ui(|shared| {
                    let Ok(mut app) = shared.try_borrow_mut() else {
                        return;
                    };
                    if !app.suggest.visible || app.suggest.rows.len() < 9 {
                        return;
                    }
                    app.suggest.active = 8;
                    app.publish_suggestions();
                });
            },
        );
        std::mem::forget(timer);
    }
}

impl App {
    /// Run a search the way the search box does, from any entry point.
    ///
    /// The palette's search row and the search field must not be two behaviours:
    /// the word is recorded in the local history, the feed owner is cleared so the
    /// page reloads instead of reusing what it had, and the box shows what was
    /// searched for.
    pub(crate) fn submit_search(shared: &Shared, word: &str) {
        let word = word.trim().to_string();
        if word.is_empty() {
            shared.borrow().notify(2, "请输入关键词");
            return;
        }
        {
            let mut app = shared.borrow_mut();
            app.suggest_close();
            app.search_word = word.clone();
            app.feed_owner = None;
            let _ = app.store.push_search(&word);
            // A history change: the chips are published from the cache now, so the
            // store write has to refresh it here (plan §2.3 R16).
            app.reload_recent_searches();
            app.ui().set_search_text(word.clone().into());
        }
        App::navigate(shared, Destination::Search);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(tag: &str, active: bool) -> SuggestionItem {
        SuggestionItem {
            tag: tag.into(),
            label: tag.into(),
            count: "".into(),
            local: false,
            active,
        }
    }

    /// One cursor move writes one row. A rebuilt model would recreate every row,
    /// and a recreated row never reports that it just became active - which is
    /// exactly the event U6's auto-scroll listens for.
    #[test]
    fn the_panel_model_is_patched_not_rebuilt() {
        let model = slint::VecModel::from(vec![item("a", false), item("b", false)]);
        assert_eq!(
            sync_suggestion_rows(&model, vec![item("a", false), item("b", false)]),
            0,
            "nothing changed, nothing is written"
        );
        assert_eq!(
            sync_suggestion_rows(&model, vec![item("a", false), item("b", true)]),
            1,
            "the cursor moved: one row"
        );
        assert!(model.row_data(1).unwrap().active);

        // A shorter list shrinks the tail, then patches what is left.
        assert_eq!(sync_suggestion_rows(&model, vec![item("a", true)]), 2);
        assert_eq!(model.row_count(), 1);
        assert!(model.row_data(0).unwrap().active);
    }
}
