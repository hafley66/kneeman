# Generic `Reduce` trait: unify the hand-written FSMs

Not SpacetimeDB, not rxjs/observables — a **one-method trait + associated types + a Store + a test
harness**, in-tree, no deps. Unifies the reducers we keep hand-writing so the store/effect/replay/test
plumbing is written once. The reactive *cell* layer already exists (`futures_signals::Mutable`); this
is the missing *transition* layer.

## Prior art in-repo (four reducers, same shape)
| reducer | where | signature today |
|---|---|---|
| sim step | `core/za_warudo.rs::reduce_next_state` | `(state, inputs) -> state`, no effects, **owned by ggrs** |
| Nav | `shell/ui/menu/router.rs:100` | `reduce(&mut self, NavCmd) -> NavOut` |
| Lobby | `net/src/lobby.rs:171` | `reduce(&mut self, Event, &mut Vec<Effect>)` |
| World (future) | — | `apply(World, &WorldEvent) -> World` |
`router.rs:6-10` already notes Nav "wants to be its own crate… reducer-pure like `smash_core::step`."

## The trait
```rust
pub trait Reduce {
    type Event;
    type Effect;
    fn reduce(&mut self, ev: Self::Event, out: &mut Vec<Self::Effect>); // self IS the state
}

pub struct Store<R: Reduce> { state: R, pending: Vec<R::Effect> }
impl<R: Reduce> Store<R> {
    pub fn dispatch(&mut self, ev: R::Event) -> Vec<R::Effect> {
        let mut o = Vec::new(); self.state.reduce(ev, &mut o); o   // shell interprets o
    }
    pub fn state(&self) -> &R { &self.state }
}
```
Matches `Lobby` verbatim. `Nav` adapts by folding `NavOut::Fire(dialog)` into an `Effect`.

## What it automates (once, for every FSM regardless of size)
- `Store<R>` holds state + hands effects to the shell.
- Generic test harness `run<R: Reduce>(r, events) -> Vec<R::Effect>` — drive an event list,
  `matches!`-classify effects (exactly what `net/tests/lobby.rs` already does by hand).
- Free snapshot/replay when `R: Clone` (Nav + Lobby already are) → rollback-testable FSMs.

## Two hard boundaries (or it rots into a framework)
1. **Leave the sim out.** `step` has no effects and ggrs already is its store/replay; wrapping it
   buys nothing. The trait is for the *effectful* FSMs (Nav, Lobby, World).
2. **Generic layer collects effects, never interprets them.** It can't know how to run `SpawnItem`
   vs `Dial`; execution stays in each shell (`DebugUi::process`, the netplay methods). Putting effect
   execution in the generic layer = a DI container. Don't.
- No streams/observables/scheduler/deps. `Mutable` = the subscribe side; this = the transition side.

## Payoff / sequencing
`plans/router-as-crate.md` and `plans/netplay-as-crate.md` both propose extracting a pure reducer —
this trait is their shared base. Extract once (~40 lines, `smash-reduce` crate or fold into an
existing small one); both extractions depend on it instead of re-inventing the harness. Build after
World lands (World is the third real consumer that justifies the abstraction — rule of three).

Sibling: plans/router-as-crate.md, plans/netplay-as-crate.md, plans/sim-as-library.md,
docs/game-architecture/event-sourcing.md (the reduce+effects lineage: Elm/redux-loop/boardgame.io).
