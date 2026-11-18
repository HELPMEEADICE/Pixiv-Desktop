use super::*;
use pixiv_types::domain::{SearchTarget, SortOrder};

// ======================================================================
// The search surface's controls (round twelve, plan §7 F1)
//
// Same file as the suggestion panel because they are the same surface: the box the
// user types into, the panel that suggests words, and the three controls that say
// which works a word returns. R11 froze the vocabulary and the Slint half and wired
// neither; this is the Rust half.
// ======================================================================

/// The search surface's controls, as one value.
///
/// R11 froze the vocabulary (SortOrder/SearchTarget each carry ALL, labels(),
/// as_param()) and the Slint half (two SegmentedControls and a related-tag strip in
/// ui/pages/gallery.slint), and wired neither: sort_chosen, mode_chosen and
/// related_tag reached nothing in src/. This is the Rust half of that seam — one
/// value the page mirrors, so the three controls cannot drift apart.
///
/// The request is built here too (query), because the control index, the parameter
/// the URL carries and the query the client is handed are three views of one
/// decision: endpoint.rs serialises order={order} and s_mode={target} from exactly
/// the two fields this type holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FeedControls {
    pub(crate) sort: SortOrder,
    pub(crate) target: SearchTarget,
    /// The "exclude AI" switch. It describes the *next* request only: the flag is
    /// read when a request is issued, so flipping the switch never rewrites what is
    /// on screen (plan §7 F4).
    pub(crate) exclude_ai: bool,
}

impl FeedControls {
    /// The sort group's name for assistive technology. A control may not invent its
    /// own label (§3, G31), so the owner of sort-labels names it.
    pub(crate) const SORT_GROUP: &'static str = "排序方式";
    /// The match-mode group's name, for the same reason.
    pub(crate) const TARGET_GROUP: &'static str = "匹配方式";
    /// How many related tags the strip offers.
    pub(crate) const RELATED_LIMIT: usize = 12;

    pub(crate) fn sort_index(self) -> i32 {
        sort_index_of(self.sort)
    }

    pub(crate) fn target_index(self) -> i32 {
        target_index_of(self.target)
    }

    /// The same controls with the order the control at "index" offers.
    ///
    /// The page reports *the index the gesture picked* and never assigns its own
    /// sort-index; this is the write that makes the report the model's.
    pub(crate) fn with_sort_index(self, index: i32) -> Self {
        Self {
            sort: sort_at(index),
            ..self
        }
    }

    pub(crate) fn with_target_index(self, index: i32) -> Self {
        Self {
            target: target_at(index),
            ..self
        }
    }

    /// The query a search request carries.
    ///
    /// "page" is the caller's: a control change is page 1 (reload_feed resets the
    /// cursor), a "load more" is the next one, and both spell the same request.
    pub(crate) fn query(self, word: &str, page: u32) -> SearchQuery {
        let mut query = SearchQuery::new(word);
        query.sort = self.sort;
        query.target = self.target;
        query.page = page.max(1);
        query
    }

    /// Drop AI-generated works from a page this request brought.
    ///
    /// Pixiv's search endpoint has no AI parameter, so "exclude AI" is a filter over
    /// the reply — applied at landing time, which is what keeps it a property of the
    /// request and not of the render: sending the switch on does not re-filter the
    /// page already on screen.
    pub(crate) fn without_ai(self, items: Vec<Illust>) -> Vec<Illust> {
        if !self.exclude_ai {
            return items;
        }
        items
            .into_iter()
            .filter(|illust| illust.ai_type == 0)
            .collect()
    }

    /// The next state when a pick asks for a different page, or None when the value
    /// on screen was picked again.
    pub(crate) fn changed(self, next: Self) -> Option<Self> {
        if next == self { None } else { Some(next) }
    }
}

/// The order at a control index, clamped to the vocabulary: an index the page
/// cannot have picked asks for the first order rather than for nothing.
fn sort_at(index: i32) -> SortOrder {
    if index < 0 {
        return SortOrder::default();
    }
    SortOrder::from_index(index as usize).unwrap_or_default()
}

fn target_at(index: i32) -> SearchTarget {
    if index < 0 {
        return SearchTarget::default();
    }
    SearchTarget::from_index(index as usize).unwrap_or_default()
}

/// Where an order sits in the control, i.e. the index the control draws as current.
pub(crate) fn sort_index_of(order: SortOrder) -> i32 {
    SortOrder::ALL
        .iter()
        .position(|known| *known == order)
        .map_or(0, |index| index as i32)
}

pub(crate) fn target_index_of(target: SearchTarget) -> i32 {
    SearchTarget::ALL
        .iter()
        .position(|known| *known == target)
        .map_or(0, |index| index as i32)
}

/// Related tags for a result page.
///
/// SearchBodyWire.related_tags (the list the web page draws) is dropped by
/// SearchResult::from_wire, which lives in a crate this track may not edit; the
/// page's own works carry the same information, so the row is built from what the
/// reply actually brought: the tags most of those works share, minus the word that
/// was searched for. Ties keep the order the tags first appeared in, so two replies
/// with the same counts draw the same row instead of reshuffling it.
pub(crate) fn related_tags(items: &[Illust], word: &str, limit: usize) -> Vec<String> {
    let searched = word.trim().to_lowercase();
    let mut counts: Vec<(String, usize)> = Vec::new();
    for illust in items {
        for tag in &illust.tags {
            let name = tag.name.trim();
            if name.is_empty() || name.to_lowercase() == searched {
                continue;
            }
            match counts.iter_mut().find(|(known, _)| known == name) {
                Some((_, count)) => *count += 1,
                None => counts.push((name.to_string(), 1)),
            }
        }
    }
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    counts
        .into_iter()
        .take(limit)
        .map(|(name, _)| name)
        .collect()
}

impl App {
    /// Put the frozen vocabulary and the current selection on the page.
    ///
    /// The values live here, never in the page: the page draws what it is handed,
    /// reports the index a gesture picked, and this writes the model back (§2.1 U28).
    pub(crate) fn publish_controls(&self) {
        let ui = self.ui();
        let sort: Vec<slint::SharedString> =
            SortOrder::labels().into_iter().map(Into::into).collect();
        ui.set_grid_sort_labels(ModelRc::from(Rc::new(VecModel::from(sort))));
        ui.set_grid_sort_index(self.controls.sort_index());
        ui.set_grid_sort_label(FeedControls::SORT_GROUP.into());
        let target: Vec<slint::SharedString> =
            SearchTarget::labels().into_iter().map(Into::into).collect();
        ui.set_grid_mode_labels(ModelRc::from(Rc::new(VecModel::from(target))));
        ui.set_grid_mode_index(self.controls.target_index());
        ui.set_grid_mode_label(FeedControls::TARGET_GROUP.into());
        ui.set_grid_exclude_ai(self.controls.exclude_ai);
    }

    /// The related-tag row of the page the reply belongs to.
    pub(crate) fn publish_related_tags(&self, tags: Vec<String>) {
        let labels: Vec<slint::SharedString> = tags.into_iter().map(Into::into).collect();
        self.ui()
            .set_grid_related_tags(ModelRc::from(Rc::new(VecModel::from(labels))));
    }

    /// A control was picked.
    ///
    /// Re-picking the index already on screen is not a new question and asks for
    /// nothing; any real change clears the loaded-feed owner, and reload_feed is
    /// where the cursor goes back to page 1 and the epoch is bumped — so a reply for
    /// the old parameters can no longer land (the guard the rest of this round's
    /// replies already use).
    pub(crate) fn control_chosen(shared: &Shared, pick: impl FnOnce(FeedControls) -> FeedControls) {
        {
            let mut app = shared.borrow_mut();
            let Some(next) = app.controls.changed(pick(app.controls)) else {
                return;
            };
            app.controls = next;
            app.feed_owner = None;
            app.publish_controls();
        }
        App::reload_feed(shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixiv_types::domain::Tag;

    fn work(id: u64, ai: u8, tags: &[&str]) -> Illust {
        Illust {
            id: pixiv_types::Id(id),
            title: String::new(),
            kind: IllustKind::Illustration,
            page_count: 1,
            width: 600,
            height: 800,
            urls: IllustUrls::default(),
            author: Author::default(),
            tags: tags.iter().map(|name| Tag::new(*name)).collect(),
            description: String::new(),
            create_date: None,
            bookmark: Bookmark::default(),
            stats: IllustStats::default(),
            x_restrict: 0,
            ai_type: ai,
            alt: String::new(),
            sl: 0,
            is_masked: false,
            is_bookmarkable: true,
        }
    }

    /// The endpoint's own query string, read from its source.
    ///
    /// endpoint::search is private to pixiv-api and this track may not edit a crate
    /// to publish it, so the contract is read where it is written. That keeps one
    /// assertion end to end — control index, as_param(), the URL the client
    /// actually requests — instead of three assertions each half proving itself.
    fn endpoint_search_template() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/pixiv-api/src/endpoint.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        source
            .lines()
            .find(|line| line.contains("&order={order}"))
            .expect("the search endpoint must build one URL")
            .trim()
            .to_string()
    }

    #[test]
    fn every_control_index_reaches_the_endpoint_url() {
        let template = endpoint_search_template();
        assert!(
            template.contains("order={order}") && template.contains("s_mode={target}"),
            "the search URL must carry the two parameters the controls fill:\n{template}"
        );
        let controls = FeedControls::default();
        for index in 0..SortOrder::ALL.len() {
            let order = SortOrder::from_index(index).expect("every index has an order");
            let query = controls.with_sort_index(index as i32).query("landscape", 1);
            let url = template
                .replace("{order}", query.sort.as_param())
                .replace("{target}", query.target.as_param());
            assert!(
                url.contains(&format!("order={}", order.as_param())),
                "index {index} must reach the URL as order={}:\n{url}",
                order.as_param()
            );
        }
        for index in 0..SearchTarget::ALL.len() {
            let target = SearchTarget::from_index(index).expect("every index has a mode");
            let query = controls
                .with_target_index(index as i32)
                .query("landscape", 1);
            let url = template
                .replace("{order}", query.sort.as_param())
                .replace("{target}", query.target.as_param());
            assert!(
                url.contains(&format!("s_mode={}", target.as_param())),
                "index {index} must reach the URL as s_mode={}:\n{url}",
                target.as_param()
            );
        }
    }

    #[test]
    fn the_labels_are_the_vocabulary_in_its_own_order() {
        let labels = SortOrder::labels();
        assert_eq!(labels.len(), SortOrder::ALL.len());
        for (index, order) in SortOrder::ALL.iter().enumerate() {
            assert_eq!(
                labels[index],
                order.label(),
                "the label list is the order list"
            );
            assert_eq!(
                FeedControls::default().with_sort_index(index as i32).sort,
                *order,
                "the control's index addresses the same array its labels came from"
            );
        }
        let modes = SearchTarget::labels();
        assert_eq!(modes.len(), SearchTarget::ALL.len());
        for (index, target) in SearchTarget::ALL.iter().enumerate() {
            assert_eq!(modes[index], target.label());
            assert_eq!(
                FeedControls::default()
                    .with_target_index(index as i32)
                    .target,
                *target
            );
        }
    }

    #[test]
    fn every_selectable_index_has_its_own_parameter() {
        let mut seen: Vec<&str> = Vec::new();
        for order in SortOrder::ALL {
            let param = order.as_param();
            assert!(
                !param.is_empty(),
                "an order without a parameter is a dead control"
            );
            assert!(
                !seen.contains(&param),
                "{param} is carried by two control indices"
            );
            seen.push(param);
        }
        let mut seen: Vec<&str> = Vec::new();
        for target in SearchTarget::ALL {
            let param = target.as_param();
            assert!(
                !param.is_empty(),
                "a mode without a parameter is a dead control"
            );
            assert!(!seen.contains(&param), "{param} is carried by two indices");
            seen.push(param);
        }
    }

    #[test]
    fn a_control_index_round_trips_through_the_index_it_draws() {
        for (index, _) in SortOrder::ALL.iter().enumerate() {
            let controls = FeedControls::default().with_sort_index(index as i32);
            assert_eq!(controls.sort_index(), index as i32);
        }
        for (index, _) in SearchTarget::ALL.iter().enumerate() {
            let controls = FeedControls::default().with_target_index(index as i32);
            assert_eq!(controls.target_index(), index as i32);
        }
        // An index past the vocabulary is not a crash and not a wrong order.
        assert_eq!(
            FeedControls::default().with_sort_index(99).sort,
            SortOrder::default()
        );
        assert_eq!(
            FeedControls::default().with_target_index(-3).target,
            SearchTarget::default()
        );
    }

    #[test]
    fn a_changed_control_asks_for_the_first_page_and_an_unchanged_one_for_nothing() {
        let current = FeedControls::default();
        assert_eq!(
            current.with_sort_index(current.sort_index()),
            current,
            "re-picking the index on screen is not a new question"
        );
        let next = current.with_sort_index(2);
        assert_ne!(next, current);
        let next = current.changed(next).expect("a new order asks again");
        assert_eq!(next.sort, SortOrder::PopularDesc);
        let query = next.query("landscape", 1);
        assert_eq!(query.sort.as_param(), "popular_d");
        assert_eq!(
            current.changed(current.with_sort_index(0)),
            None,
            "the page already on screen asked the question the pick repeats"
        );
    }

    #[test]
    fn the_query_carries_the_controls_and_the_word() {
        let controls = FeedControls {
            sort: SortOrder::DateAsc,
            target: SearchTarget::TitleCaption,
            exclude_ai: false,
        };
        let query = controls.query("landscape", 3);
        assert_eq!(query.word, "landscape");
        assert_eq!(query.page, 3);
        assert_eq!(query.sort.as_param(), "date_a");
        assert_eq!(query.target.as_param(), "s_tc");
        // Page zero is not a page: the cursor starts at one and a clamp keeps a bug
        // in it from asking Pixiv for p=0.
        assert_eq!(controls.query("w", 0).page, 1);
    }

    #[test]
    fn related_tags_rank_by_how_many_works_share_them_and_skip_the_searched_word() {
        let items = vec![
            work(1, 0, &["landscape", "桜", "空"]),
            work(2, 0, &["landscape", "空"]),
            work(3, 0, &["landscape", "猫"]),
        ];
        assert_eq!(
            related_tags(&items, "landscape", 3),
            vec!["空".to_string(), "桜".to_string(), "猫".to_string()],
            "the two works that share 空 rank above the two singletons, which keep \
             first-seen order"
        );
        assert_eq!(
            related_tags(&items, "landscape", 1),
            vec!["空".to_string()],
            "the limit is the strip's width, and it is a limit"
        );
    }

    #[test]
    fn an_empty_result_offers_no_related_tags() {
        assert!(
            related_tags(&[], "landscape", FeedControls::RELATED_LIMIT).is_empty(),
            "an empty tag list is what keeps the strip off the page"
        );
        let one = vec![work(1, 0, &["landscape"])];
        assert!(
            related_tags(&one, "landscape", FeedControls::RELATED_LIMIT).is_empty(),
            "a page whose only tag is the searched word offers nothing to click"
        );
    }

    #[test]
    fn the_ai_switch_filters_the_reply_and_never_the_page_on_screen() {
        let items = vec![work(1, 0, &[]), work(2, 1, &[]), work(3, 2, &[])];
        let off = FeedControls::default();
        assert_eq!(
            off.without_ai(items.clone()).len(),
            3,
            "the switch off keeps every work"
        );
        let on = FeedControls {
            exclude_ai: true,
            ..off
        };
        let kept: Vec<u64> = on
            .without_ai(items)
            .into_iter()
            .map(|illust| illust.id.get())
            .collect();
        assert_eq!(
            kept,
            vec![1],
            "AI-assisted (2) is AI too, not only AI-generated"
        );
    }

    /// F4: the switch is not a query parameter — the endpoint has none — so it is
    /// the reply that carries it. The two halves are asserted together so the flag
    /// cannot silently become a URL field nobody sends.
    #[test]
    fn f4_the_ai_switch_is_not_a_url_parameter_and_filters_the_reply() {
        let template = endpoint_search_template();
        assert!(
            !template.contains("ai="),
            "Pixiv's search URL has no AI parameter to carry:\n{template}"
        );
        let works = vec![work(1, 0, &[]), work(2, 1, &[])];
        let on = FeedControls {
            exclude_ai: true,
            ..FeedControls::default()
        };
        assert_eq!(on.without_ai(works.clone()).len(), 1);
        assert_eq!(
            FeedControls::default().without_ai(works).len(),
            2,
            "the switch off keeps the page as it arrived"
        );
    }

    #[test]
    fn the_same_value_twice_is_one_state() {
        let controls = FeedControls {
            sort: SortOrder::PopularDesc,
            target: SearchTarget::TagPartial,
            exclude_ai: true,
        };
        assert_eq!(controls, controls.with_sort_index(2).with_target_index(1));
        let mut flipped = controls;
        flipped.exclude_ai = false;
        assert_ne!(flipped, controls, "the AI switch is part of the state");
    }
}

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
        if self.mode == Mode::Demo {
            let query = self.suggest.query.to_lowercase();
            let recent: Vec<String> = self
                .recent_words
                .iter()
                .filter(|term| term.to_lowercase().contains(&query))
                .cloned()
                .collect();
            let mut rows = pixiv_types::domain::history_suggestions(&recent, 5);
            let tags = self
                .demo
                .trending_tags()
                .into_iter()
                .filter(|tag| tag.to_lowercase().contains(&query) && !recent.contains(tag))
                .take(10 - rows.len())
                .map(|tag| Suggestion {
                    tag,
                    ..Suggestion::default()
                });
            rows.extend(tags);
            self.suggest.note = if rows.is_empty() {
                "没有匹配的演示标签，按回车直接搜索".into()
            } else {
                String::new()
            };
            self.suggest.rows = rows;
            self.suggest.active = -1;
            self.suggest.visible = true;
            self.publish_suggestions();
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
                        app.tasks.clone(),
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
                                app.suggest.note = format!(
                                    "联想服务不可用（{}），以上为本机搜索记录",
                                    describe_session_error(&error)
                                );
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
mod control_tests {
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
