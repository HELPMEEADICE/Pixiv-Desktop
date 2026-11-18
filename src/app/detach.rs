//! Releasing every callback closure the shell holds.
//!
//! This module is the answer to one measured fact: at the moment the event loop
//! returns, `Rc::strong_count(app)` is **114** - one for the app's own `Shared`, one
//! for `thread_local APP`, and one for each of the 112 `ui.on_*(move |..| { let shared
//! = app.clone(); .. })` registrations in `src/app/callbacks/*` and the modules that
//! install their own. Those closures live inside the `AppWindow`, and the `AppWindow`
//! outlives `ui.run()`, so the tokio runtime was still owned by 112 references that
//! nothing could reach. The process's teardown depended on the order the component
//! finally unwound in - which is why an exit could leave a process behind.
//!
//! Slint's `on_*` setters *replace* the handler, so calling each one with an empty
//! closure drops the previous closure and its `Rc` while the component is still alive
//! and the UI thread is still ours. After [`detach_callbacks`] returns, the App is
//! reachable exactly once - from the shutdown sequence - and its drop parks the
//! runtime deterministically.
//!
//! The table is generated from the shell's generated bindings and is asserted against
//! them by `the_detacher_table_covers_every_shell_callback`: a callback added to
//! `ui/main.slint` without a line here fails the build's tests rather than silently
//! retaining a reference.

use super::*;
fn detach_on_apply_template(ui: &AppWindow) {
    ui.on_apply_template(|_a| {});
}
fn detach_on_batch_bookmark(ui: &AppWindow) {
    ui.on_batch_bookmark(|| {});
}
fn detach_on_batch_download(ui: &AppWindow) {
    ui.on_batch_download(|| {});
}
fn detach_on_card_clicked(ui: &AppWindow) {
    ui.on_card_clicked(|_a, _b, _c| {});
}
fn detach_on_card_focused(ui: &AppWindow) {
    ui.on_card_focused(|_a, _b| {});
}
fn detach_on_choose_directory(ui: &AppWindow) {
    ui.on_choose_directory(|| {});
}
fn detach_on_clear_cache(ui: &AppWindow) {
    ui.on_clear_cache(|| {});
}
fn detach_on_clear_downloads(ui: &AppWindow) {
    ui.on_clear_downloads(|| {});
}
fn detach_on_clear_history(ui: &AppWindow) {
    ui.on_clear_history(|| {});
}
fn detach_on_clear_selection(ui: &AppWindow) {
    ui.on_clear_selection(|| {});
}
fn detach_on_detail_back(ui: &AppWindow) {
    ui.on_detail_back(|| {});
}
fn detach_on_detail_download_all(ui: &AppWindow) {
    ui.on_detail_download_all(|| {});
}
fn detach_on_detail_download_page(ui: &AppWindow) {
    ui.on_detail_download_page(|| {});
}
fn detach_on_detail_export_ugoira(ui: &AppWindow) {
    ui.on_detail_export_ugoira(|| {});
}
fn detach_on_detail_follow(ui: &AppWindow) {
    ui.on_detail_follow(|| {});
}
fn detach_on_detail_open_author(ui: &AppWindow) {
    ui.on_detail_open_author(|| {});
}
fn detach_on_detail_open_external(ui: &AppWindow) {
    ui.on_detail_open_external(|| {});
}
fn detach_on_detail_open_tag(ui: &AppWindow) {
    ui.on_detail_open_tag(|_a| {});
}
fn detach_on_detail_reload_image(ui: &AppWindow) {
    ui.on_detail_reload_image(|| {});
}
fn detach_on_detail_request_pan(ui: &AppWindow) {
    ui.on_detail_request_pan(|_a, _b, _c, _d| {});
}

fn detach_on_detail_viewport_changed(ui: &AppWindow) {
    ui.on_detail_viewport_changed(|_a, _b| {});
}
fn detach_on_detail_request_toggle_zoom(ui: &AppWindow) {
    ui.on_detail_request_toggle_zoom(|_a, _b| {});
}
fn detach_on_detail_request_zoom(ui: &AppWindow) {
    ui.on_detail_request_zoom(|_a, _b, _c, _d, _e| {});
}
fn detach_on_detail_retry(ui: &AppWindow) {
    ui.on_detail_retry(|| {});
}
fn detach_on_detail_reveal(ui: &AppWindow) {
    ui.on_detail_reveal(|| {});
}
fn detach_on_detail_select_page(ui: &AppWindow) {
    ui.on_detail_select_page(|_a| {});
}
fn detach_on_detail_set_actual_size(ui: &AppWindow) {
    ui.on_detail_set_actual_size(|| {});
}
fn detach_on_detail_set_fit_mode(ui: &AppWindow) {
    ui.on_detail_set_fit_mode(|_a| {});
}
fn detach_on_detail_set_zoom(ui: &AppWindow) {
    ui.on_detail_set_zoom(|_a| {});
}
fn detach_on_detail_toggle_bookmark(ui: &AppWindow) {
    ui.on_detail_toggle_bookmark(|| {});
}
fn detach_on_dismiss_notice(ui: &AppWindow) {
    ui.on_dismiss_notice(|| {});
}
fn detach_on_download_cancel(ui: &AppWindow) {
    ui.on_download_cancel(|_a| {});
}
fn detach_on_download_choose_directory(ui: &AppWindow) {
    ui.on_download_choose_directory(|| {});
}
fn detach_on_download_clear_finished(ui: &AppWindow) {
    ui.on_download_clear_finished(|| {});
}
fn detach_on_download_open_folder(ui: &AppWindow) {
    ui.on_download_open_folder(|| {});
}
fn detach_on_download_open_task(ui: &AppWindow) {
    ui.on_download_open_task(|_a| {});
}
fn detach_on_download_retry(ui: &AppWindow) {
    ui.on_download_retry(|_a| {});
}
fn detach_on_download_retry_all(ui: &AppWindow) {
    ui.on_download_retry_all(|| {});
}
fn detach_on_go_back(ui: &AppWindow) {
    ui.on_go_back(|| {});
}
fn detach_on_go_forward(ui: &AppWindow) {
    ui.on_go_forward(|| {});
}
fn detach_on_grid_action(ui: &AppWindow) {
    ui.on_grid_action(|| {});
}
fn detach_on_grid_measured(ui: &AppWindow) {
    ui.on_grid_measured(|_a| {});
}
fn detach_on_grid_retry(ui: &AppWindow) {
    ui.on_grid_retry(|| {});
}
fn detach_on_grid_scrolled(ui: &AppWindow) {
    ui.on_grid_scrolled(|_a| {});
}
fn detach_on_load_more(ui: &AppWindow) {
    ui.on_load_more(|| {});
}
fn detach_on_local_forget(ui: &AppWindow) {
    ui.on_local_forget(|_a| {});
}
fn detach_on_local_open(ui: &AppWindow) {
    ui.on_local_open(|_a| {});
}
fn detach_on_local_redownload(ui: &AppWindow) {
    ui.on_local_redownload(|_a| {});
}
fn detach_on_local_reveal(ui: &AppWindow) {
    ui.on_local_reveal(|_a| {});
}
fn detach_on_navigate(ui: &AppWindow) {
    ui.on_navigate(|_a| {});
}
fn detach_on_novel_external(ui: &AppWindow) {
    ui.on_novel_external(|| {});
}
fn detach_on_novel_load_more(ui: &AppWindow) {
    ui.on_novel_load_more(|| {});
}
fn detach_on_novel_open(ui: &AppWindow) {
    ui.on_novel_open(|_a| {});
}
fn detach_on_novel_open_author(ui: &AppWindow) {
    ui.on_novel_open_author(|| {});
}
fn detach_on_novel_open_chapter(ui: &AppWindow) {
    ui.on_novel_open_chapter(|_a| {});
}
fn detach_on_novel_open_tag(ui: &AppWindow) {
    ui.on_novel_open_tag(|_a| {});
}
fn detach_on_novel_read(ui: &AppWindow) {
    ui.on_novel_read(|| {});
}
fn detach_on_novel_refresh(ui: &AppWindow) {
    ui.on_novel_refresh(|| {});
}
fn detach_on_novel_retry(ui: &AppWindow) {
    ui.on_novel_retry(|| {});
}
fn detach_on_novel_search(ui: &AppWindow) {
    ui.on_novel_search(|_a| {});
}
fn detach_on_novel_toggle_bookmark(ui: &AppWindow) {
    ui.on_novel_toggle_bookmark(|| {});
}
fn detach_on_open_author(ui: &AppWindow) {
    ui.on_open_author(|_a| {});
}
fn detach_on_open_data_folder(ui: &AppWindow) {
    ui.on_open_data_folder(|| {});
}
fn detach_on_open_illust(ui: &AppWindow) {
    ui.on_open_illust(|_a| {});
}
fn detach_on_open_profile(ui: &AppWindow) {
    ui.on_open_profile(|_a| {});
}
fn detach_on_open_project(ui: &AppWindow) {
    ui.on_open_project(|| {});
}
fn detach_on_open_tag(ui: &AppWindow) {
    ui.on_open_tag(|_a| {});
}
fn detach_on_page_step(ui: &AppWindow) {
    ui.on_page_step(|_a| {});
}
fn detach_on_palette_choose(ui: &AppWindow) {
    ui.on_palette_choose(|_a| {});
}
fn detach_on_palette_close(ui: &AppWindow) {
    ui.on_palette_close(|| {});
}
fn detach_on_palette_query(ui: &AppWindow) {
    ui.on_palette_query(|_a| {});
}
fn detach_on_pause_task(ui: &AppWindow) {
    ui.on_pause_task(|_a| {});
}
fn detach_on_pick_restrict(ui: &AppWindow) {
    ui.on_pick_restrict(|_a| {});
}
fn detach_on_profile_back(ui: &AppWindow) {
    ui.on_profile_back(|| {});
}
fn detach_on_profile_follow(ui: &AppWindow) {
    ui.on_profile_follow(|| {});
}
fn detach_on_profile_load_more(ui: &AppWindow) {
    ui.on_profile_load_more(|| {});
}
fn detach_on_profile_refresh(ui: &AppWindow) {
    ui.on_profile_refresh(|| {});
}
fn detach_on_profile_retry(ui: &AppWindow) {
    ui.on_profile_retry(|| {});
}
fn detach_on_reader_back(ui: &AppWindow) {
    ui.on_reader_back(|| {});
}
fn detach_on_reader_chapter(ui: &AppWindow) {
    ui.on_reader_chapter(|_a| {});
}
fn detach_on_reader_open_external(ui: &AppWindow) {
    ui.on_reader_open_external(|| {});
}
fn detach_on_reader_prefs_edit(ui: &AppWindow) {
    ui.on_reader_prefs_edit(|_a, _b, _c| {});
}
fn detach_on_reader_retry(ui: &AppWindow) {
    ui.on_reader_retry(|| {});
}
fn detach_on_reader_scrolled(ui: &AppWindow) {
    ui.on_reader_scrolled(|_a, _b| {});
}
fn detach_on_reader_toggle_chrome(ui: &AppWindow) {
    ui.on_reader_toggle_chrome(|| {});
}
fn detach_on_reader_toggle_settings(ui: &AppWindow) {
    ui.on_reader_toggle_settings(|| {});
}
fn detach_on_resume_task(ui: &AppWindow) {
    ui.on_resume_task(|_a| {});
}
fn detach_on_search_edited(ui: &AppWindow) {
    ui.on_search_edited(|_a| {});
}
fn detach_on_search_exclude_ai_toggled(ui: &AppWindow) {
    ui.on_search_exclude_ai_toggled(|_a| {});
}
fn detach_on_search_mode_chosen(ui: &AppWindow) {
    ui.on_search_mode_chosen(|_a| {});
}
fn detach_on_search_related_tag_chosen(ui: &AppWindow) {
    ui.on_search_related_tag_chosen(|_a| {});
}
fn detach_on_search_sort_chosen(ui: &AppWindow) {
    ui.on_search_sort_chosen(|_a| {});
}
fn detach_on_search_submitted(ui: &AppWindow) {
    ui.on_search_submitted(|_a| {});
}
fn detach_on_select_all(ui: &AppWindow) {
    ui.on_select_all(|| {});
}
fn detach_on_select_ranking_filter(ui: &AppWindow) {
    ui.on_select_ranking_filter(|_a| {});
}
fn detach_on_select_theme_mode(ui: &AppWindow) {
    ui.on_select_theme_mode(|_a| {});
}
fn detach_on_shell_shortcut(ui: &AppWindow) {
    ui.on_shell_shortcut(|_a| {});
}
fn detach_on_sign_in_browser(ui: &AppWindow) {
    ui.on_sign_in_browser(|| {});
}
fn detach_on_sign_out(ui: &AppWindow) {
    ui.on_sign_out(|| {});
}
fn detach_on_submit_cookie(ui: &AppWindow) {
    ui.on_submit_cookie(|| {});
}
fn detach_on_suggest_chosen(ui: &AppWindow) {
    ui.on_suggest_chosen(|_a| {});
}
fn detach_on_suggest_dismissed(ui: &AppWindow) {
    ui.on_suggest_dismissed(|| {});
}
fn detach_on_suggest_key(ui: &AppWindow) {
    ui.on_suggest_key(|_a| {});
}
fn detach_on_toggle_bookmark(ui: &AppWindow) {
    ui.on_toggle_bookmark(|_a| {});
}
fn detach_on_toggle_demo(ui: &AppWindow) {
    ui.on_toggle_demo(|_a| {});
}
fn detach_on_toggle_fullscreen(ui: &AppWindow) {
    ui.on_toggle_fullscreen(|| {});
}
fn detach_on_toggle_pause_all(ui: &AppWindow) {
    ui.on_toggle_pause_all(|| {});
}
fn detach_on_toggle_playback(ui: &AppWindow) {
    ui.on_toggle_playback(|| {});
}
fn detach_on_toggle_theme(ui: &AppWindow) {
    ui.on_toggle_theme(|| {});
}
fn detach_on_width_changed(ui: &AppWindow) {
    ui.on_width_changed(|_a| {});
}

/// One detacher per shell callback.
const DETACHERS: &[fn(&AppWindow)] = &[
    detach_on_apply_template,
    detach_on_batch_bookmark,
    detach_on_batch_download,
    detach_on_card_clicked,
    detach_on_card_focused,
    detach_on_choose_directory,
    detach_on_clear_cache,
    detach_on_clear_downloads,
    detach_on_clear_history,
    detach_on_clear_selection,
    detach_on_detail_back,
    detach_on_detail_download_all,
    detach_on_detail_download_page,
    detach_on_detail_export_ugoira,
    detach_on_detail_follow,
    detach_on_detail_open_author,
    detach_on_detail_open_external,
    detach_on_detail_open_tag,
    detach_on_detail_reload_image,
    detach_on_detail_request_pan,
    detach_on_detail_viewport_changed,
    detach_on_detail_request_toggle_zoom,
    detach_on_detail_request_zoom,
    detach_on_detail_retry,
    detach_on_detail_reveal,
    detach_on_detail_select_page,
    detach_on_detail_set_actual_size,
    detach_on_detail_set_fit_mode,
    detach_on_detail_set_zoom,
    detach_on_detail_toggle_bookmark,
    detach_on_dismiss_notice,
    detach_on_download_cancel,
    detach_on_download_choose_directory,
    detach_on_download_clear_finished,
    detach_on_download_open_folder,
    detach_on_download_open_task,
    detach_on_download_retry,
    detach_on_download_retry_all,
    detach_on_go_back,
    detach_on_go_forward,
    detach_on_grid_action,
    detach_on_grid_measured,
    detach_on_grid_retry,
    detach_on_grid_scrolled,
    detach_on_load_more,
    detach_on_local_forget,
    detach_on_local_open,
    detach_on_local_redownload,
    detach_on_local_reveal,
    detach_on_navigate,
    detach_on_novel_external,
    detach_on_novel_load_more,
    detach_on_novel_open,
    detach_on_novel_open_author,
    detach_on_novel_open_chapter,
    detach_on_novel_open_tag,
    detach_on_novel_read,
    detach_on_novel_refresh,
    detach_on_novel_retry,
    detach_on_novel_search,
    detach_on_novel_toggle_bookmark,
    detach_on_open_author,
    detach_on_open_data_folder,
    detach_on_open_illust,
    detach_on_open_profile,
    detach_on_open_project,
    detach_on_open_tag,
    detach_on_page_step,
    detach_on_palette_choose,
    detach_on_palette_close,
    detach_on_palette_query,
    detach_on_pause_task,
    detach_on_pick_restrict,
    detach_on_profile_back,
    detach_on_profile_follow,
    detach_on_profile_load_more,
    detach_on_profile_refresh,
    detach_on_profile_retry,
    detach_on_reader_back,
    detach_on_reader_chapter,
    detach_on_reader_open_external,
    detach_on_reader_prefs_edit,
    detach_on_reader_retry,
    detach_on_reader_scrolled,
    detach_on_reader_toggle_chrome,
    detach_on_reader_toggle_settings,
    detach_on_resume_task,
    detach_on_search_edited,
    detach_on_search_exclude_ai_toggled,
    detach_on_search_mode_chosen,
    detach_on_search_related_tag_chosen,
    detach_on_search_sort_chosen,
    detach_on_search_submitted,
    detach_on_select_all,
    detach_on_select_ranking_filter,
    detach_on_select_theme_mode,
    detach_on_shell_shortcut,
    detach_on_sign_in_browser,
    detach_on_sign_out,
    detach_on_submit_cookie,
    detach_on_suggest_chosen,
    detach_on_suggest_dismissed,
    detach_on_suggest_key,
    detach_on_toggle_bookmark,
    detach_on_toggle_demo,
    detach_on_toggle_fullscreen,
    detach_on_toggle_pause_all,
    detach_on_toggle_playback,
    detach_on_toggle_theme,
    detach_on_width_changed,
];

/// Drop every callback closure registered on the window.
///
/// Called once, at the start of the shutdown sequence, before the App is dropped.
pub(crate) fn detach_callbacks(app: &Shared) {
    let ui = match app.try_borrow() {
        Ok(app) => app.ui.upgrade(),
        Err(_) => {
            tracing::warn!("the app was borrowed; its callbacks could not be released");
            None
        }
    };
    let Some(ui) = ui else {
        // The window is already gone, which means its callbacks went with it.
        return;
    };
    for detach in DETACHERS {
        detach(&ui);
    }
}

#[cfg(test)]
mod tests {

    /// The detacher table must name every callback the shell declares.
    ///
    /// This is the guard on the fix, not a style check. Each `ui.on_*` call site in
    /// `src/app/**` captures an `Rc<RefCell<App>>`; a callback that is registered but
    /// never detached keeps the App - and through it the tokio runtime - alive past
    /// the end of the event loop, which is precisely the defect that left a process
    /// behind after the window closed. Measured before the fix: 114 strong references
    /// at the moment shutdown began.
    ///
    /// The source of truth is the app itself, because that is where a new callback is
    /// added. Reading the generated bindings would be circular: a callback declared in
    /// Slint but never installed costs nothing at exit.
    #[test]
    fn the_detacher_table_covers_every_installed_callback() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut installed: Vec<String> = Vec::new();
        collect_installed(&root.join("src").join("app"), &mut installed);
        installed.sort();
        installed.dedup();
        assert!(
            installed.len() > 50,
            "the scan found {} registrations, which cannot be the application layer",
            installed.len()
        );

        let table = std::fs::read_to_string(root.join("src").join("app").join("detach.rs"))
            .expect("detach.rs is part of the tree");
        let mut detached: Vec<String> = Vec::new();
        for line in table.lines() {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix("ui.on_") else {
                continue;
            };
            if let Some((name, _)) = rest.split_once('(') {
                detached.push(format!("on_{name}"));
            }
        }
        detached.sort();
        detached.dedup();

        let missing: Vec<&String> = installed.iter().filter(|n| !detached.contains(n)).collect();
        assert!(
            missing.is_empty(),
            "these callbacks are installed but never detached, so each one keeps an Rc to the App alive past exit: {missing:?}"
        );
        let extra: Vec<&String> = detached.iter().filter(|n| !installed.contains(n)).collect();
        assert!(
            extra.is_empty(),
            "these detachers name callbacks the app never installs, so they release nothing: {extra:?}"
        );
    }

    /// Walk the app sources for `ui.on_*(` registrations.
    ///
    /// Test modules are skipped: a test that registers a callback on a fixture window
    /// is not a registration the application has to release.
    fn collect_installed(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_installed(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // `detach.rs` *contains* `ui.on_` text; it is the table, not a caller.
            if path.file_name().and_then(|n| n.to_str()) == Some("detach.rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            let tests_from = lines
                .iter()
                .enumerate()
                .find_map(|(index, line)| {
                    if !line.trim_start().starts_with("#[cfg(test)]") {
                        return None;
                    }
                    let next = lines.get(index + 1)?.trim();
                    next.starts_with("mod tests").then_some(index)
                })
                .unwrap_or(usize::MAX);
            for (index, line) in lines.iter().enumerate() {
                if index >= tests_from {
                    continue;
                }
                let trimmed = line.trim();
                if trimmed.starts_with("//") {
                    continue;
                }
                let mut rest = *line;
                while let Some(at) = rest.find("ui.on_") {
                    let after = &rest[at + "ui.".len()..];
                    let name: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    if after[name.len()..].starts_with('(') && !name.is_empty() {
                        out.push(name);
                    }
                    rest = &rest[at + "ui.on_".len()..];
                }
            }
        }
    }
}
