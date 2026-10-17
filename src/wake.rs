//! The desktop interface wake, wrapping fastframe-shell's egui waker.
//!
//! The shell's waker attaches to and detaches from the window; the backend
//! only needs [`Wake::wake`]. This forwards the shell's methods and
//! implements the core wake trait, so one handle serves both.

use spotifast_core::wake::Wake;

#[derive(Clone, Default)]
pub struct Waker(pub fastframe_shell::Waker);

impl Waker {
    pub fn wake(&self) {
        self.0.wake();
    }

    pub fn attach(&self, ctx: &egui::Context) {
        self.0.attach(ctx);
    }

    pub fn detach(&self) {
        self.0.detach();
    }
}

impl Wake for Waker {
    fn wake(&self) {
        self.0.wake();
    }
}
