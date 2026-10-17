//! The desktop adapter for `core::engine::UiContext`: the egui context the
//! engine's window and clipboard effects run against.

use spotifast_core::engine::UiContext;

pub struct EguiUiContext(pub egui::Context);

impl UiContext for EguiUiContext {
    fn fullscreen(&self, on: bool) {
        self.0
            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    fn maximized(&self, on: bool) {
        self.0
            .send_viewport_cmd(egui::ViewportCommand::Maximized(on));
    }

    fn minimized(&self, on: bool) {
        self.0
            .send_viewport_cmd(egui::ViewportCommand::Minimized(on));
    }

    fn focus(&self) {
        self.0.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn close(&self) {
        self.0.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn copy_text(&self, text: String) {
        self.0.copy_text(text);
    }

    fn open_url(&self, url: String) {
        self.0.open_url(egui::OpenUrl::new_tab(url));
    }

    fn include_image(&self, uri: String, bytes: Vec<u8>) {
        self.0.include_bytes(uri, bytes);
    }

    fn repaint(&self) {
        self.0.request_repaint();
    }

    fn forget_images(&self) {
        self.0.forget_all_images();
    }

    fn viewport_fullscreen(&self) -> bool {
        self.0.input(|input| input.viewport().fullscreen.unwrap_or(false))
    }

    fn viewport_maximized(&self) -> bool {
        self.0.input(|input| input.viewport().maximized.unwrap_or(false))
    }
}
