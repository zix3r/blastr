# Blastr

Blastr shows what a Rust code change can reach, and why. It compares two revisions of a Cargo
project, classifies what changed structurally, and follows the references backwards to the code
and tests the change can affect, with the evidence path for every item. The analysis comes from
rust-analyzer and runs locally, in the Blastr window: these tools need that window open (run
`blastr`), and what they open, compare or select is shown there to the user.

## Concepts

- **Comparison**: two sides, a base and a head. Usually `HEAD` against the working tree, or a
  commit against its parent, or a range `main..HEAD`.
- **Changes**: structural differences between the sides, each with a kind: `body`, `signature`,
  `visibility`, `added`, `removed`, `renamed`, `field +/-/~`, `variant +/-/~`, `trait-item`,
  `impl`, `const-value`, `type-alias`, `macro-def`, `attribute`, `manifest`, `non-semantic`
  (formatting and comments only). Changes are numbered from 0; tools take that number.
- **Affected items**, in two classes:
  - *needs editing*: code that refers to something whose shape changed (a removed field, a new
    variant in an exhaustive `match`, a changed signature). It no longer fits the change.
  - *may behave differently*: code that still compiles but runs through changed code, directly
    or through other functions.
- **Confidence**: `definite` (every step resolved), `likely`, `possible` (a step is a
  candidate: a call through a trait object or generic bound that may dispatch to the changed
  code, or a target rust-analyzer could not pin to one definition).
- **Distance**: the number of steps from the change to the item; 1 means it uses the changed
  symbol directly.
- **Evidence paths**: up to three per item, each a chain of steps `item -> ... -> changed
  symbol`, every step with the file:line where the reference is written. The first step says why
  the rule applied ("calls vat_rate, whose signature changed").
- **Limits**: what the result may miss. Read them before trusting a short list.
  - unresolved references: code rust-analyzer could not resolve; anything reached only through
    it is missing ("near the changes" ones matter most).
  - candidate steps / coarse rules: some items may not really be affected.
  - build scripts and procedural macros not run (restricted), macros not expanded, failed
    queries: generated code is missing from the graph.
  - change in inactive code: a change under a `cfg` that is off for these options; its impact
    is not traced. `set_options` can enable features or another target.
  - items below the confidence minimum or in ignored files: left out of the list on purpose.
  - submodules and Git LFS content are not analysed.
- **Tests to run**: the tests that changed or that a change reaches, as `cargo test` commands
  per target, with how many of the target's tests they select.

## Workflows

- **Review the current comparison**: `status`, then `get_analysis`; `list_impact` for more
  items (filter by change or class); `explain_impact` for why one is listed; `tests_to_run`.
- **Another comparison**: `compare` with `worktree`, `staged`, a revision (`HEAD~1`, a hash) or
  a range (`main..HEAD`); it waits for the result and returns the summary.
- **Work in progress**: with the working tree compared, every saved file refreshes the result
  in the window. A `get_analysis` after saving files waits for that refresh and returns the
  fresh result. Clients that subscribe to `blastr://analysis` are told when it changes.
- **Plan a change**: `what_if` with a symbol and the change to assume (body, signature,
  removed, visibility, field or variant changes) traces what it would reach, without touching
  any file. The window shows the what-if until `compare` is called.
- **Pick tests**: `tests_to_run`, optionally for one change.
- **Point the user at something**: `show` selects a change or a symbol in the window.

Symbols are named by their path, `crate::module::Type::method`; a unique shorter suffix such as
`Type::method` also works. `find_symbol` looks a name up.

## Tools

- `guide`: this text.
- `status`: whether Blastr is open, the repository, comparison, options, and whether a run is
  in progress.
- `open_repository(path)`: opens a repository in the window; the user may be asked whether to
  trust it.
- `compare(spec)`: chooses the comparison and returns the analysis summary.
- `set_options(features, all_features, target, ignore)`: the repository's analysis options;
  runs the comparison again.
- `get_analysis`: the current result: comparison, changes, counts, tests, top items, limits.
- `list_impact(change, class, min_confidence, offset, limit)`: affected items, paged, each with
  its one-line reason.
- `explain_impact(symbol)`: the evidence paths of one item, step by step, and the tests that
  reach it.
- `tests_to_run(change)`: `cargo test` commands and the selected tests with their reasons.
- `find_symbol(query, limit)`: symbols matching a name.
- `what_if(symbol, assume)`: traces an assumed change of one symbol.
- `show(symbol | change)`: selects it in the window.
