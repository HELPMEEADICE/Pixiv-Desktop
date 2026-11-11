slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.set_status_text("toolchain gate ok".into());
    ui.run()
}
