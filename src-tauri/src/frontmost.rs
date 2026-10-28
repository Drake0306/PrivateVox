//! Name of the app in front, used to label history rows.

/// Localized name of the frontmost application, if it can be determined.
#[cfg(target_os = "macos")]
pub fn frontmost_app_name() -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    let ws = NSWorkspace::sharedWorkspace();
    let app = ws.frontmostApplication()?;
    app.localizedName().map(|n| n.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_app_name() -> Option<String> {
    None
}
