// Pixiv Desktop — native Rust + Slint client.
//
// Keep `main` thin: everything interesting lives in `app`.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod images;
mod layout;
mod picker;
mod route;
mod session;
mod zoom;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,pixiv=info")),
        )
        .with_target(false)
        .init();

    app::run()
}
