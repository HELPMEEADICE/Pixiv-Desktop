// ---------------------------------------------------------------- callbacks

use super::*;

mod detail;
mod downloads;
mod feed;
mod profile;
mod search;
mod settings;
mod shell;

/// Wire every Slint callback to the handler family that owns it.
pub(crate) fn install_callbacks(app: &Shared) {
    shell::install(app);
    search::install(app);
    detail::install(app);
    profile::install(app);
    feed::install(app);
    settings::install(app);
    downloads::install(app);
}
