//! UI themes. A `Theme` is the chrome contract the menu draws through: a global stylesheet hook
//! plus a titled window + the button and nav-rail widgets. Two impls live here: `dark::Dark` (the
//! debug panel's look) and `xp::Xp` (Windows XP "Luna", worn by the menu). The menu is generic over
//! `T: Theme`, so the choice is monomorphized -- no dyn vtable.

pub mod dark;
pub mod xp;

pub trait Theme {
    /// Install a global egui stylesheet (run once at ctx setup). Default: keep egui's defaults.
    fn install(&self, _ctx: &egui::Context) {}

    /// A titled top-level container, centered on screen. `add` fills the body below the title bar.
    fn window(&self, ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui));

    /// A modal dialog: scrim over the base, centered framed body. `add` fills it.
    fn dialog(&self, ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui));

    /// A primary push button. Returns its response.
    fn button(&self, ui: &mut egui::Ui, label: &str) -> egui::Response;

    /// A nav-rail entry; `selected` marks the current page.
    fn nav_item(&self, ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response;
}
