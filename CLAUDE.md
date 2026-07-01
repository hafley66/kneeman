
<!-- BEGIN: sprefa-dl -->
## Querying this codebase with `dl`

`dl` (sprefa v5) is datalog over code. Reach for it instead of grep when you need
structured facts: call graph, import/type graph, blast radius, lint rails, codemods.

- Run a program: `dl prog.dl --root .` (prints `?` rows). `--no-daemon` for ad-hoc.
- Discovery rail: `dl --check` runs every `.dl/*.dl`; exits 2 on a `diag` row.
- The `.dl/dl-self-lint.dl` rail makes a broken/mistyped `.dl` a `--check` failure
  (the engine lints `.dl` via the built-in `dl_diag` relation, like rust-analyzer).
- Surface reference: see the engine's generated `docs/reference/{relations,functions,syntax,examples}.md`.
- `agent_edit`/`agent_touch` are git-free (keyed on `--root` dir); `changed`/`created` need git.
- `dl setup --project` wired a `dl --hook` PostToolUse hook in `.claude/settings.json`:
  a `.dl/` rule heading `inject`/`inject_skill`/`block` fires on a matching tool use
  (see `.dl/hook-skill-on-test.dl`). Editor-independent context injection, no bash glue.
- It also wired a `.githooks/pre-commit` (`dl --check`) + `core.hooksPath`, so a
  `diag` rail in `.dl/*.dl` blocks a bad commit (`git commit -n` bypasses).
- Live editor squiggles: `dl setup --vscode` installs the bundled LSP extension.

See the `sprefa-dl` skill for the full surface and authoring gotchas.
<!-- END: sprefa-dl -->
