//! The portable application engine: the state the views read and the actions
//! they ask for, minus the desktop's window, tray, and media wiring.
//!
//! `src/app.rs`'s `App` today mixes this state with the egui/fastframe shell.
//! The extraction moves the state and its transitions here, so both the
//! desktop and an Android front end drive the same engine. The one seam back
//! into the front end is [`UiContext`], for the window and clipboard effects a
//! UI action asks for: the desktop adapts `egui::Context`, Android adapts its
//! own surface (or no-ops where there is no equivalent).

/// Window and clipboard effects a UI action asks for, supplied by the front
/// end. `Send` because action handling runs on the interface thread while the
/// backend runs on its own.
pub trait UiContext: Send {
    /// Set the window's fullscreen state.
    fn fullscreen(&self, on: bool);
    /// Set the window's maximized state.
    fn maximized(&self, on: bool);
    /// Restore a minimized window (`false`) or minimize it (`true`).
    fn minimized(&self, on: bool);
    /// Focus the window.
    fn focus(&self);
    /// Close the window.
    fn close(&self);
    /// Copy text to the clipboard.
    fn copy_text(&self, text: String);
    /// Open a URL in the default browser.
    fn open_url(&self, url: String);
    /// Register image bytes so the front end can draw them.
    fn include_image(&self, uri: String, bytes: Vec<u8>);
    /// Ask for another frame.
    fn repaint(&self);
    /// Drop every image the front end holds.
    fn forget_images(&self);
    /// Whether the window is currently fullscreen.
    fn viewport_fullscreen(&self) -> bool;
    /// Whether the window is currently maximized.
    fn viewport_maximized(&self) -> bool;
}

/// A front-end context with no window or clipboard, for headless runs and
/// tests that never draw.
pub struct NoUiContext;

impl UiContext for NoUiContext {
    fn fullscreen(&self, _on: bool) {}
    fn maximized(&self, _on: bool) {}
    fn minimized(&self, _on: bool) {}
    fn focus(&self) {}
    fn close(&self) {}
    fn copy_text(&self, _text: String) {}
    fn open_url(&self, _url: String) {}
    fn include_image(&self, _uri: String, _bytes: Vec<u8>) {}
    fn repaint(&self) {}
    fn forget_images(&self) {}
    fn viewport_fullscreen(&self) -> bool {
        false
    }
    fn viewport_maximized(&self) -> bool {
        false
    }
}
