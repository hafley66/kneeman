//! The menu as a memory-router state machine, split the way the sim is: a PURE reducer over nav
//! state ([`Nav::reduce`], no I/O, no cells, no egui) plus an impure effect layer ([`Router`]) that
//! interprets app [`Intent`]s against the game cells. Screens never write cells -- they push Intents
//! into a Vec, drained AFTER the egui frame (egui can't poke Godot mid-draw), so the route has one writer.
//!
//! Footnote: the [`Nav`] half wants to be its own crate. It is a generic string state machine of
//! positional inputs (routes = the path) and unpositional inputs (dialogs = query-param overlays);
//! parameterized over the app's `Route`/`Dialog` types it is a reusable router with no game ties.
//! Reducer-pure like `smash_core::step`, so it would be trivially testable + rollback-friendly. Kept
//! concrete + in-tree for now; extract once a second consumer wants it. See plans/router-as-crate.md.

use futures_signals::signal::Mutable;

use crate::net::NetDebug;
use crate::sim::{self, ItemKind, SimState, StrokeId, ToolKind, Tune};

// ============================ pure nav reducer (extractable crate) ============================

/// The base page. `CharEdit` carries its slot the way a URL path carries a positional param.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Closed, // in game; the menu is not shown
    Home,
    Items,
    Charss, // character select (never "css")
    CharEdit { slot: u8 },
    Rules,
    Background,
    Feel,
    Controls,
    Network,
}

/// A modal laid over the base (reachable from many bases). Single confirm: one button fires it, one
/// cancels. No per-player gate — a dialog either fires or it doesn't.
#[derive(Clone, Copy, PartialEq)]
pub enum Dialog {
    ConfirmReset,
    GifImport,
}

#[derive(Clone, Copy, PartialEq)]
pub struct DialogState {
    pub kind: Dialog,
}

/// Where the menu is: a base page plus an optional dialog on top. Copy + small, so it is cheap to
/// snapshot and (later) to serialize for rollback tests.
#[derive(Clone, Copy, PartialEq)]
pub struct Location {
    pub base: Route,
    pub dialog: Option<DialogState>,
}

/// One nav transition. The only inputs the pure reducer understands. App effects (spawn an item,
/// reset tune) are NOT here -- they live in [`Intent`] and run in the effect layer.
pub enum NavCmd {
    Esc, // context-sensitive: cancel dialog, else back, else open the menu
    Push(Route),
    Back,
    OpenDialog(Dialog),
    Confirm, // fire the open dialog
    CancelDialog,
}

/// What the reducer hands back: nothing, or "this dialog passed its gate -- caller, run its effect".
/// Routing stays pure; the dialog's actual consequence is the caller's to interpret.
#[derive(Clone, Copy, PartialEq)]
pub enum NavOut {
    None,
    Fire(Dialog),
}

/// The pure nav state: current location, a history stack for Back, and the dialog gate setting.
/// `reduce` is a total function of (state, cmd) -> state with no side effects.
#[derive(Clone)]
pub struct Nav {
    loc: Location,
    history: Vec<Route>,
}

impl Default for Nav {
    fn default() -> Self {
        Self {
            loc: Location {
                base: Route::Closed,
                dialog: None,
            },
            history: Vec::new(),
        }
    }
}

impl Nav {
    pub fn location(&self) -> Location {
        self.loc
    }

    /// Apply one nav command. Pure: mutates only `self`, returns any dialog that passed its gate.
    pub fn reduce(&mut self, cmd: NavCmd) -> NavOut {
        match cmd {
            NavCmd::Esc => {
                if self.loc.dialog.is_some() {
                    self.loc.dialog = None;
                } else if matches!(self.loc.base, Route::Closed) {
                    self.push(Route::Home); // in game -> open the pause menu
                } else {
                    self.back(); // in a menu -> back out (Home backs to Closed = resume)
                }
                NavOut::None
            }
            NavCmd::Push(r) => {
                self.push(r);
                NavOut::None
            }
            NavCmd::Back => {
                self.back();
                NavOut::None
            }
            NavCmd::OpenDialog(d) => {
                self.loc.dialog = Some(DialogState { kind: d });
                NavOut::None
            }
            NavCmd::CancelDialog => {
                self.loc.dialog = None;
                NavOut::None
            }
            NavCmd::Confirm => self.confirm(),
        }
    }

    fn push(&mut self, r: Route) {
        self.history.push(self.loc.base);
        self.loc = Location {
            base: r,
            dialog: None,
        };
    }

    fn back(&mut self) {
        let base = self.history.pop().unwrap_or(Route::Closed);
        self.loc = Location { base, dialog: None };
    }

    /// Confirm the open dialog: clear it and report it for firing.
    fn confirm(&mut self) -> NavOut {
        let Some(ds) = self.loc.dialog.as_ref() else {
            return NavOut::None;
        };
        let kind = ds.kind;
        self.loc.dialog = None;
        NavOut::Fire(kind)
    }
}

// ================================ impure effect layer (shell) =================================

/// A read-only snapshot the screens see: the game cells sampled once this frame, plus the current
/// route (for nav highlighting).
pub struct MenuCtx<'a> {
    pub state: &'a SimState,
    pub tune: &'a Tune,
    pub charsel: [i64; 2],
    pub route: Route,
    pub net: &'a NetDebug, // transport snapshot for the Network page
    pub lobbies: &'a [crate::net::LobbyRow], // shell-held lobby list for the Network page's grid
    pub push_status: &'a str, // web-push opt-in status ("pinging for '<room>'"), mirrored from JS
}

/// The writable cells the effect layer drains into. Borrowed from KneeMan for the frame.
pub struct MenuCells<'a> {
    pub state: &'a Mutable<SimState>,
    pub tune: &'a Mutable<Tune>,
    pub charsel: &'a Mutable<[i64; 2]>,
    pub net: &'a Mutable<NetDebug>,
}

/// What a screen asks for: nav edges (routed through the pure reducer) + app effects (run on cells).
pub enum Intent {
    Esc,
    Nav(Route),
    Back,
    OpenDialog(Dialog),
    DialogConfirm,
    DialogCancel,
    /// Spawn a menu-card item: kind + the pen loadout (tool, stroke registry row; guns ignore them).
    SpawnItem(ItemKind, ToolKind, StrokeId),
    ClearItems,
    SetChar { slot: usize, idx: i64 },
    /// Signal to the shell (`DebugUi::process`) to open the egui debug panel. The pure Router
    /// treats this as a no-op; the shell intercepts and drains it before calling `Router::apply`.
    OpenDebugPanel,
    /// Network page actions. Like `OpenDebugPanel`, the pure Router no-ops these; the shell
    /// intercepts them before `Router::apply` and drives the KneeMan netplay methods.
    FindMatch,
    LeaveMatch,
    /// Versioned lobby browser (also shell-intercepted, Router no-ops). `OpenLobby` hosts a room for
    /// the current version; `JoinLobby` dials an existing one by its key. Wired to the mesh in P2.
    OpenLobby,
    JoinLobby(String),
    /// Web-push opt-in (shell-intercepted): the shell calls the JS bridge to run the subscribe flow.
    PushSubscribe,
}

/// Wraps the pure [`Nav`] and interprets [`Intent`]s: nav edges go through `Nav::reduce`, app edges
/// hit the game cells. A dialog that passes its gate fires its app effect here.
#[derive(Default)]
pub struct Router {
    nav: Nav,
}

impl Router {
    pub fn location(&self) -> Location {
        self.nav.location()
    }

    /// Drain a frame's intents, after the egui pass.
    pub fn apply(&mut self, intents: Vec<Intent>, cells: &MenuCells) {
        for it in intents {
            self.apply_one(it, cells);
        }
    }

    fn apply_one(&mut self, intent: Intent, cells: &MenuCells) {
        match intent {
            Intent::Esc => self.dispatch(NavCmd::Esc, cells),
            Intent::Nav(r) => self.dispatch(NavCmd::Push(r), cells),
            Intent::Back => self.dispatch(NavCmd::Back, cells),
            Intent::OpenDialog(d) => self.dispatch(NavCmd::OpenDialog(d), cells),
            Intent::DialogCancel => self.dispatch(NavCmd::CancelDialog, cells),
            Intent::DialogConfirm => self.dispatch(NavCmd::Confirm, cells),
            Intent::SpawnItem(k, tool, stroke) => spawn_item(cells, k, tool, stroke),
            Intent::ClearItems => clear_items(cells),
            Intent::SetChar { slot, idx } => {
                if slot < 2 {
                    let mut c = cells.charsel.get();
                    c[slot] = idx;
                    cells.charsel.set(c);
                }
            }
            // Intercepted by the shell before Router::apply is called; pure Router ignores them.
            Intent::OpenDebugPanel
            | Intent::FindMatch
            | Intent::LeaveMatch
            | Intent::OpenLobby
            | Intent::JoinLobby(_)
            | Intent::PushSubscribe => {}
        }
    }

    /// Run a nav command through the pure reducer; if a dialog passed its gate, fire its effect.
    fn dispatch(&mut self, cmd: NavCmd, cells: &MenuCells) {
        if let NavOut::Fire(d) = self.nav.reduce(cmd) {
            self.fire_dialog(d, cells);
        }
    }

    fn fire_dialog(&mut self, kind: Dialog, cells: &MenuCells) {
        match kind {
            Dialog::ConfirmReset => cells.tune.set(Tune::default()),
            Dialog::GifImport => { /* wired with the gif-background feature */ }
        }
    }
}

fn spawn_item(cells: &MenuCells, kind: ItemKind, tool: ToolKind, stroke: StrokeId) {
    let mut s = cells.state.get();
    sim::spawn_kind(&mut s, kind, tool, stroke, &cells.tune.get());
    cells.state.set(s);
}

fn clear_items(cells: &MenuCells) {
    let mut s = cells.state.get();
    sim::clear_items(&mut s);
    cells.state.set(s);
}
