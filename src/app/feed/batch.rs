//! Batch actions on a selection (round nine, F2).
//!
//! The two verbs share one shape: decide what may be queued here, on the UI
//! thread (the caps and the queue's own skip rule), then do the network work off
//! it and apply the outcome once.

use super::*;

/// How many works one batch may cover. Fifty is the plan's cap; a bigger
/// selection is refused with a sentence rather than truncated silently.
pub(crate) const BATCH_CAP: usize = 50;
/// How many page lists may be in flight at once. Three is the same bound the
/// detail page uses for decodes and the queue uses for downloads.
const BATCH_PAGES: usize = 3;
/// The setting the bookmark rows read for their visibility. The detail track
/// writes it; the batch reads it, so the two cannot disagree.
pub(crate) const BOOKMARK_RESTRICT_KEY: &str = "bookmark_restrict";

/// Why this batch cannot run, if it cannot.
pub(crate) fn cap_violation(count: usize) -> Option<String> {
    (count > BATCH_CAP)
        .then(|| format!("批量操作最多 {BATCH_CAP} 件作品，当前选中 {count} 件；请分批进行"))
}

/// The requests one work needs, from its page list.
///
/// A work with no page list has exactly one image, and it is the largest the
/// payload offered - the same rule `enqueue_download` follows for a single work,
/// so a batch and a single download of the same work queue the same files.
pub(crate) fn requests_for_work(
    id: &str,
    title: &str,
    author: &str,
    page_count: u32,
    largest: &str,
    pages: &[PageMeta],
) -> Vec<DownloadRequest> {
    if pages.is_empty() {
        return vec![DownloadRequest {
            illust_id: id.to_string(),
            title: title.to_string(),
            author: author.to_string(),
            page_index: 0,
            page_count: page_count.max(1),
            url: largest.to_string(),
            extension: "jpg".to_string(),
        }];
    }
    pages
        .iter()
        .map(|page| DownloadRequest {
            illust_id: id.to_string(),
            title: title.to_string(),
            author: author.to_string(),
            page_index: page.index,
            page_count: pages.len().max(1) as u32,
            url: page.urls.original.clone(),
            extension: extension_for(&page.urls.original),
        })
        .collect()
}

/// The identity of one queued page: a work and which of its pages.
pub(crate) fn queue_key(request: &DownloadRequest) -> (String, u32) {
    (request.illust_id.clone(), request.page_index)
}

/// The pages the queue already holds (finished tasks are not a reason to skip:
/// re-downloading a deleted file is legitimate).
pub(crate) fn queued_keys(tasks: &[pixiv_download::DownloadTask]) -> HashSet<(String, u32)> {
    tasks
        .iter()
        .filter(|task| !task.state.is_terminal())
        .map(|task| queue_key(&task.request))
        .collect()
}

/// Drop what the queue already holds - the existing skip rule, unchanged.
pub(crate) fn fresh_requests(
    requests: Vec<DownloadRequest>,
    queued: &HashSet<(String, u32)>,
) -> Vec<DownloadRequest> {
    requests
        .into_iter()
        .filter(|request| !queued.contains(&queue_key(request)))
        .collect()
}

/// One line for the status bar and the InfoBar: how many works of the batch are
/// in the queue, and why the rest are not.
pub(crate) fn batch_summary(
    total: usize,
    queued_works: usize,
    skipped_works: usize,
    failed: &[String],
) -> String {
    let mut line = format!("批量下载 {queued_works}/{total}");
    if skipped_works > 0 {
        line.push_str(&format!("，{skipped_works} 件已在队列中"));
    }
    if !failed.is_empty() {
        line.push_str(&format!("，{} 件失败：{}", failed.len(), failed.join("；")));
    }
    line
}

pub(crate) fn bookmark_summary(total: usize, done: usize, failed: &[String]) -> String {
    let mut line = format!("批量收藏 {done}/{total}");
    if !failed.is_empty() {
        line.push_str(&format!("，{} 件失败：{}", failed.len(), failed.join("；")));
    }
    line
}

/// The stored visibility, defaulting to public when the setting was never written.
pub(crate) fn bookmark_restrict_from(value: Option<&str>) -> BookmarkRestrict {
    match value.map(str::trim) {
        Some("1") | Some("private") | Some("Private") => BookmarkRestrict::Private,
        _ => BookmarkRestrict::Public,
    }
}

impl App {
    // ------------------------------------------------------------- batch download

    /// Queue every page of every selected work.
    ///
    /// The page lists are fetched here, bounded by `BATCH_PAGES`, because the
    /// work payload itself does not carry them; the queue is only touched back on
    /// the UI thread, where the skip rule can see what is already in it.
    pub(crate) fn batch_download(shared: &Shared) {
        let (ids, mode, client, rt) = {
            let app = shared.borrow();
            (
                app.selection.ids.clone(),
                app.mode,
                app.client.clone(),
                app.rt.clone(),
            )
        };
        if ids.is_empty() {
            shared.borrow().notify(2, "先选择要下载的作品");
            return;
        }
        if let Some(message) = cap_violation(ids.len()) {
            shared.borrow().notify(2, message);
            return;
        }
        if mode == Mode::Demo {
            shared
                .borrow()
                .notify(2, "演示模式不写入文件；登录并切换到真实会话后可用");
            return;
        }
        let Some(client) = client else {
            shared.borrow().notify(3, "没有可用的网络客户端");
            return;
        };
        let total = ids.len();
        shared
            .borrow()
            .ui()
            .set_status_text(format!("批量下载 0/{total}").into());

        rt.spawn(async move {
            let mut plan: Vec<(u64, Result<Vec<PageMeta>, String>)> = Vec::with_capacity(total);
            let mut stream = futures::stream::iter(ids.into_iter().map(|id| {
                let client = client.clone();
                async move {
                    let pages = client
                        .illust_pages(Id(id))
                        .await
                        .map_err(|error| error.to_string());
                    (id, pages)
                }
            }))
            .buffer_unordered(BATCH_PAGES);
            while let Some(row) = stream.next().await {
                plan.push(row);
            }
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.enqueue_batch(plan);
            });
        });
    }

    /// Turn the fetched page lists into queue requests, skipping what is already
    /// queued and counting what could not be planned at all.
    pub(crate) fn enqueue_batch(&mut self, plan: Vec<(u64, Result<Vec<PageMeta>, String>)>) {
        let total = plan.len();
        let queued = queued_keys(&self.downloads.tasks());
        let mut requests: Vec<DownloadRequest> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        let mut queued_works = 0;
        let mut skipped_works = 0;
        for (id, pages) in plan {
            let Some(illust) = self.items.iter().find(|item| item.id.get() == id) else {
                failed.push(format!("{id}：不在当前列表"));
                continue;
            };
            let pages = match pages {
                Ok(pages) => pages,
                Err(error) => {
                    failed.push(format!("{id}：{error}"));
                    continue;
                }
            };
            let work = requests_for_work(
                &illust.id.to_string(),
                &illust.title,
                &illust.author.name,
                illust.page_count.max(1),
                illust.urls.largest(),
                &pages,
            );
            let work = fresh_requests(work, &queued);
            if work.is_empty() {
                skipped_works += 1;
            } else {
                queued_works += 1;
                requests.extend(work);
            }
        }
        let count = requests.len();
        if count > 0 {
            self.downloads.enqueue(requests);
            self.push_downloads();
        }
        let summary = batch_summary(total, queued_works, skipped_works, &failed);
        self.ui().set_status_text(summary.clone().into());
        let kind = if failed.is_empty() { 1 } else { 2 };
        self.notify(kind, summary);
    }

    // ------------------------------------------------------------- batch bookmark

    /// Bookmark every selected work, one request after another.
    ///
    /// Sequential on purpose: fifty parallel writes to one account is exactly the
    /// shape that gets rate-limited, and the reply for each work is what decides
    /// the local state - never the request.
    pub(crate) fn batch_bookmark(shared: &Shared) {
        let (ids, mode, client, rt, restrict) = {
            let app = shared.borrow();
            let stored = app.store.get_setting(BOOKMARK_RESTRICT_KEY).ok().flatten();
            (
                app.selection.ids.clone(),
                app.mode,
                app.client.clone(),
                app.rt.clone(),
                bookmark_restrict_from(stored.as_deref()),
            )
        };
        if ids.is_empty() {
            shared.borrow().notify(2, "先选择要收藏的作品");
            return;
        }
        if let Some(message) = cap_violation(ids.len()) {
            shared.borrow().notify(2, message);
            return;
        }
        if mode == Mode::Demo {
            shared
                .borrow()
                .notify(2, "批量收藏需要登录的真实会话；演示模式不提交收藏");
            return;
        }
        if !shared.borrow().session.is_authenticated() {
            shared
                .borrow()
                .notify(2, "需要登录后才能收藏，请到设置页登录");
            return;
        }
        let Some(client) = client else {
            shared.borrow().notify(3, "没有可用的网络客户端");
            return;
        };
        let total = ids.len();
        shared
            .borrow()
            .ui()
            .set_status_text(format!("批量收藏 0/{total}").into());

        rt.spawn(async move {
            let mut results: Vec<(u64, Result<pixiv_types::BookmarkState, String>)> =
                Vec::with_capacity(total);
            for id in ids {
                let outcome = client
                    .set_bookmark(Id(id), true, restrict)
                    .await
                    .map_err(|error| error.to_string());
                results.push((id, outcome));
            }
            on_ui(move |shared| {
                let Ok(mut app) = shared.try_borrow_mut() else {
                    return;
                };
                app.apply_batch_bookmarks(results);
            });
        });
    }

    /// Adopt the server's verdict per work, then say how many landed.
    pub(crate) fn apply_batch_bookmarks(
        &mut self,
        results: Vec<(u64, Result<pixiv_types::BookmarkState, String>)>,
    ) {
        let total = results.len();
        let mut done = 0;
        let mut failed: Vec<String> = Vec::new();
        for (id, outcome) in results {
            match outcome {
                // The state the server reported, not the one that was asked for: a
                // refused POST must not leave the grid claiming a bookmark.
                Ok(state) => {
                    self.apply_bookmark_state(Id(id), state);
                    done += 1;
                }
                Err(error) => failed.push(format!("{id}：{error}")),
            }
        }
        let summary = bookmark_summary(total, done, &failed);
        self.ui().set_status_text(summary.clone().into());
        let kind = if failed.is_empty() { 1 } else { 2 };
        self.notify(kind, summary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(index: usize, url: &str) -> PageMeta {
        PageMeta {
            urls: pixiv_types::domain::PageUrls {
                thumb_mini: String::new(),
                small: String::new(),
                regular: format!("{url}-regular"),
                original: url.to_string(),
            },
            index: index as u32,
            width: 1000,
            height: 1000,
        }
    }

    /// Fifty works are a batch, fifty-one are a refusal with a sentence: silently
    /// truncating would download a set the user never chose.
    #[test]
    fn the_batch_cap_is_fifty_and_says_so() {
        assert!(cap_violation(0).is_none());
        assert!(
            cap_violation(BATCH_CAP).is_none(),
            "the cap itself is allowed"
        );
        let message = cap_violation(BATCH_CAP + 1).expect("one more is refused");
        assert!(message.contains("50"), "{message}");
        assert!(message.contains("51"), "{message}");
    }

    /// The queue's existing skip rule, applied to a batch: a page already in the
    /// queue is not queued twice, and a *finished* task is no reason to skip.
    #[test]
    fn pages_already_queued_are_skipped() {
        let pages = vec![page(0, "a-p0"), page(1, "a-p1")];
        let requests = requests_for_work("42", "t", "a", 2, "a-largest", &pages);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].page_index, 1);
        assert_eq!(requests[1].extension, "jpg");

        let mut queued: HashSet<(String, u32)> = HashSet::new();
        queued.insert(queue_key(&requests[0]));
        let fresh = fresh_requests(requests, &queued);
        assert_eq!(fresh.len(), 1, "the queued page is not queued again");
        assert_eq!(fresh[0].page_index, 1);
    }

    /// A work the payload gave no page list for still downloads - one file, the
    /// largest size it named - which is what the single download does too.
    #[test]
    fn a_work_without_a_page_list_queues_its_largest_image() {
        let requests = requests_for_work("7", "t", "a", 3, "largest.jpg", &[]);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, "largest.jpg");
        assert_eq!(requests[0].page_index, 0);
        assert_eq!(requests[0].page_count, 3);
    }

    /// The one line both surfaces show: works queued over works chosen, then what
    /// was skipped and what failed.
    #[test]
    fn the_summary_names_the_counts_and_the_failures() {
        let quiet = batch_summary(4, 4, 0, &[]);
        assert_eq!(quiet, "批量下载 4/4");
        let noisy = batch_summary(4, 2, 1, &["9：网络错误".to_string()]);
        assert!(noisy.starts_with("批量下载 2/4"), "{noisy}");
        assert!(noisy.contains("1 件已在队列中"), "{noisy}");
        assert!(noisy.contains("9：网络错误"), "{noisy}");
        assert!(bookmark_summary(3, 2, &[]).starts_with("批量收藏 2/3"));
    }

    /// The bookmark visibility comes from the store, defaults to public, and
    /// survives the values a settings page might write.
    #[test]
    fn the_bookmark_restrict_setting_is_read_conservatively() {
        assert_eq!(bookmark_restrict_from(None), BookmarkRestrict::Public);
        assert_eq!(bookmark_restrict_from(Some("0")), BookmarkRestrict::Public);
        assert_eq!(
            bookmark_restrict_from(Some("public")),
            BookmarkRestrict::Public
        );
        assert_eq!(bookmark_restrict_from(Some("1")), BookmarkRestrict::Private);
        assert_eq!(
            bookmark_restrict_from(Some(" 1 ")),
            BookmarkRestrict::Private
        );
        assert_eq!(
            bookmark_restrict_from(Some("private")),
            BookmarkRestrict::Private
        );
        // Garbage is not private: the visible default is the safe one.
        assert_eq!(
            bookmark_restrict_from(Some("yes")),
            BookmarkRestrict::Public
        );
    }
}
