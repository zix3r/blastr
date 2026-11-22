# blastr-validate

The accuracy harness of Blastr. It answers two questions on a real Cargo project:

- **Compiler check** — does Blastr list the places the compiler rejects? Every
  case applies one seeded edit to one definition, runs Blastr on the original
  and the edited project, and runs `cargo check --all-targets --message-format=json`
  on the edited copy. The symbols with compiler errors are what Blastr should
  have listed as *needs editing*. Errors Blastr did not list are **misses**;
  items Blastr listed that compile fine are **false alarms**.
- **Test check** — does Blastr select the tests a change breaks? Every case seeds
  one mutation into one function body, keeps it only if it compiles, and runs
  `cargo test --workspace --no-fail-fast`. Every test that then fails must be
  among the tests Blastr selects for the same edit. The share of the suite the
  selection covers is recorded next to it.

Both checks also count, for every case, what plain reverse reachability from the
change would return — "everything that transitively depends on it" — so the
report shows how much shorter Blastr's list is than a reference search.

## Building

The harness is a Cargo project of its own: it is **not** a member of Blastr's
workspace (Blastr's root `Cargo.toml` lists `validation/harness` under
`exclude`), so `cargo build`, `cargo test` and `cargo clippy --workspace` in
Blastr never compile it. Its path dependencies point into `crates/`, and it
pins `ra_ap_syntax` to the same version as Blastr.

Point `CARGO_TARGET_DIR` at Blastr's `target/` to reuse the dependencies Blastr
has already compiled; a separate target directory rebuilds rust-analyzer, which
takes minutes.

```sh
cd validation/harness

# build and test the harness itself
CARGO_TARGET_DIR=../../target cargo build
CARGO_TARGET_DIR=../../target cargo test
CARGO_TARGET_DIR=../../target cargo clippy --all-targets -- -D warnings

# the compiler check on one project
CARGO_TARGET_DIR=../../target cargo run --release -- compiler ../projects/shopfront

# three cases only, and the evidence for one of them
CARGO_TARGET_DIR=../../target cargo run --release -- compiler ../projects/shopfront --limit 3
CARGO_TARGET_DIR=../../target cargo run --release -- compiler ../projects/shopfront --case 012-remove-field-shopfront-domain-Order-total

# the test check
CARGO_TARGET_DIR=../../target cargo run --release -- tests ../projects/shopfront

# one test case under a tuned setting, with the reason for every miss
CARGO_TARGET_DIR=../../target cargo run --release -- tests ../projects/shopfront \
  --case 004-swap-operator-billing-discount-percent_text \
  --params depth=8,decay=0.8,candidate=0.4,floor=0.05

# both checks over every project under validation/projects, then the report
CARGO_TARGET_DIR=../../target cargo run --release -- report

# tune the analysis parameters against stored results
CARGO_TARGET_DIR=../../target cargo run --release -- tune --projects shopfront,pipeline --confirm macrokit
```

`report` writes `validation/results/<project>-<check>.json` and
`validation/results/report.md`; `--cached` reuses the JSON files instead of
running the checks again, `--projects a,b` picks the projects, `--limit` and
`--seed` take a sample, and `--help` prints the options. The last full run over
the three sample projects is committed as `validation/results/report.md`, next to
the per-case JSON it was rendered from.

`tune` writes `validation/results/tuning.json` and `validation/results/tuning.md`
and is described in [Tuning the parameters](#tuning-the-parameters).

Every case result records the file, the item, the edit kind and the text of the
item before and after the edit, so a miss can be reproduced and inspected with
`--case <id>`. Case ids are `NNN-<kind>-<symbol>`, numbered in the order the
graph lists the definitions, so two runs of the same project plan the same cases.

`--params depth=8,decay=0.8,candidate=0.4,floor=0.05` runs a check with those
`AnalysisParams` instead of the defaults; a parameter the spec omits keeps its
default, and the JSON report records the set that was used. It is how a case is
rerun under a setting the sweep picked, and the JSON of that run is the evidence
for the case. `--params` belongs to `compiler` and `tests`; `report` and `tune`
keep the defaults and their own settings.

`--case` on the test check prints, for every missed test, the shortest chain of
edges that leads from the test to the changed function. A chain the plain search
has and Blastr's selection does not points at a rule that does not follow one of
the edges on it; no chain at all points at an edge the extraction never saw. The
line names the case's gap without anyone having to read the graph by hand.

## How the edits are made

Both checks cut their edit out of a real syntax tree, parsed with
`ra_ap_syntax` at the version Blastr itself uses. The graph gives the byte range
of the definition; the harness finds the syntax node with exactly that range and
replaces one part of it:

| kind | edit |
|---|---|
| `add-parameter` | `fn f(a: i64)` → `fn f(a: i64, __blastr_extra: i64)` |
| `change-return-type` | `-> T` → `-> (T,)` |
| `remove-field` | the field, its comma and its line disappear |
| `retype-field` | a field's `T` → `(T,)` |
| `add-field` | `__blastr_extra: u8` is added to a struct |
| `add-variant` | `__BlastrExtra` is added to an enum (not one with discriminants) |
| `remove-variant` | one variant disappears |
| `add-required-trait-method` | `fn __blastr_required(&self);` is added to a trait |
| `remove-function` | a free function with a body disappears (not a trait method, not `main`) |
| `narrow-visibility` | `pub` → `pub(crate)`, `pub(crate)` → private |

A generator returns nothing when the item cannot take the edit (a unit function
has no return type to change, a tuple struct has no field list, an opaque
`impl Trait` return cannot be moved into a tuple), and the edit is applied to the
text and parsed again before the case is planned: a case whose result does not
parse is dropped, not run.

The mutations of the test check come from the same trees, one per function, the
first that applies: swap an arithmetic or comparison operator (`+`/`-`,
`*`/`/`, `<`/`>=`, `==`/`!=`, `&&`/`||`), negate an `if` condition, replace a
returned value with `Default::default()` (only in a function that declares a
return type), or delete a call statement together with its line.

## How an error is mapped to a symbol

The compiler speaks in byte ranges; Blastr speaks in symbols. The bridge is the
graph of the **edited** revision: the harness hands the same edited text to
rust-analyzer (as an in-memory variant of the loaded project) and to
`cargo check` (as a file in a copy of the project), so the two agree on every
offset. Every symbol of that graph knows the byte range of its whole definition,
and the enclosing symbol of an error is the symbol with the smallest range that
still contains the first byte of the error's primary span. Smallest, because
ranges nest — a method sits inside an `impl`, the `impl` inside a module, all in
the same file — and the innermost one is the item that has to be edited.

An error in a file that is not part of the project (a dependency, the standard
library) is counted as *outside*, and an error inside a project file but outside
every symbol as *unmapped*; both are reported per case rather than guessed at.
An error whose primary span lies in the body of a macro from another crate — a
comparison in `assert_eq!`, say — is mapped through the macro's expansion chain
to the call site in the project, which is where a person would edit.
The changed definition itself is excused on both sides: it is allowed to break
inside its own body, and that is not what the list is for.

## Performance

Loading a project through rust-analyzer costs seconds, so it is done **once per
project**, not once per case. The harness loads the project into
`blastr_core::ProjectBase`, which extracts the graph of the original and then any
number of edited variants, each with one in-memory file replacement; the base
text is put back before the next variant, so the variants cannot leak into each
other. `ProjectBase` is a general addition to `blastr-core` (with its own test
there), not harness-only logic.

The compiler side is the other half: the project is copied once into a temporary
directory, each case writes its edited file into the copy, runs one
`cargo check --all-targets --offline --message-format=json` with a per-project
`CARGO_TARGET_DIR`, and puts the original text back. The copy's manifest is
never touched, which is why the overlay stays valid for every case. One cold and
one warm check of the unchanged project are run first, so a case's cost is
measured against a warm target directory and the dependency build is not
attributed to the first case.

Measured with the three sample projects, release build of the harness, Windows,
rustc 1.100.0, rust-analyzer 0.0.358: 3.5–4.3 s to load a project and extract its
graph, then 0.09–0.39 s per case for Blastr (an edited extraction plus the diff
and the impact analysis) and 0.14–0.20 s per case for `cargo check`, against
0.05 s for an unchanged warm check. The test check adds a full
`cargo test --workspace` per mutant, 1.1–1.8 s, against 0.2 s for the baseline
suite. Loading is paid once per check per project, so a 50-case compiler check of
a 5 000-line workspace costs about 20 s in all and a 50-case test check about
two minutes. The very first run on a machine with a cold file cache took 30 s to
load a 130-line project, almost all of it rust-analyzer's definition-map priming.
The report records all of these numbers per project and per check.

`cargo test --workspace` dominates the test check; a project whose tests are slow
to build makes the check slow, which is why every Cargo run has a time limit and
the timeouts are counted as exclusions.

## Tuning the parameters

`tune` searches the analysis parameters (`blastr_model::AnalysisParams`: depth,
decay, candidate weight, class weights, score floor, minimum confidence, ignore)
for a setting that loses fewer true items and lists less.

Both checks spend most of their time in Cargo, but Cargo's verdict of a case —
which symbols it rejects, which tests it makes fail — does not depend on Blastr's
parameters. Only Blastr's own analysis does. So the sweep does not run the checks
again: it reads the per-case results of an earlier `report` as the **ground
truth**, replans the same cases from the projects as they are on disk (a case is
matched to its stored result by id, which the plan makes stable), and re-runs
only the impact analysis. A case's edited graph does not depend on the parameters
either, so each case is extracted once and the impact analysis runs once per grid
point on that graph.

The other half of a case's cost is the compiler's judgement of items whose target
it never built. The stored result does not record the compiler's output, but
everything it *did* expect or list was in a target that was built, so the crate
roots of those symbols are taken as the confirmed targets: an item in one of them
is scored, an item anywhere else is counted as unconfirmed and left out of both
sides, exactly as the check does. Stored cases that were excluded stay excluded.

| option | meaning |
|---|---|
| `--projects a,b` | the projects to tune on (default `shopfront`, `pipeline`) |
| `--confirm p` | a held-out project; the winner and the current defaults are scored on it and both are reported |
| `--grid SPEC` | `decay=0.5,0.6;candidate=0.3,0.4;floor=0.01,0.05;depth=6,10` — the cartesian product of the values given; a parameter the spec omits keeps its default |
| `--results DIR` | where the stored `<project>-{compiler,tests}.json` are (default `validation/results`) |
| `--out DIR` | where `tuning.json` and `tuning.md` go (default `validation/results`) |
| `--limit N` | at most N cases per check and project: a smoke test, marked as such in the report |

The default grid is 39 settings: decay {0.5, 0.6, 0.7, 0.8}, score floor
{0.01, 0.02, 0.05} and depth {6, 8, 10} crossed in full — the two cut-offs and
the scale they compare against — plus the candidate weight {0.3, 0.4, 0.6, 0.8}
swept at the default floor and depth. The full four-way cross (144 settings) is
one `--grid` away.

### Which setting wins

For every setting the sweep adds up, over every scored case of every tuning
project: the compiler errors Blastr did not list plus the failing tests it did not
select (**misses**), the false alarms, the size of Blastr's list and the number of
selected tests. The rule is the plan's:

1. fewest misses — no true item lost to the cut-offs,
2. then the fewest items listed,
3. then the fewest selected tests,
4. then the setting closest to the current defaults (the sum of the relative
   changes of depth, decay, candidate weight and score floor).

Because 1 and 2 trade against each other, the report also carries the **Pareto
front**: every setting that no other setting beats in misses, list size and
selected tests at once. A setting that lists half as much but costs one miss is
on that front even though the rule does not pick it.

### What it does not tune

The class weights and the minimum confidence stay at their defaults:`possible` is
already the widest confidence (nothing can be lost by raising it, so the sweep
could only make the list longer), and a class weight only scales the score the
floor is compared with, which the decay, candidate weight and floor sweep already
covers.

The **rename threshold** is `blastr_core::diff::MatchParams::rename_similarity`
(0.7, `MatchParams::default()`), used by `diff::classify`. The sweep cannot move
it: `ProjectBase::variant` compares with `MatchParams::default()` and takes no
match parameters. It would also change nothing on these cases — no edit kind the
harness plans renames a symbol, so no case contains a rename. Exercising it would
need a new edit kind (`rename-symbol`) and a general way to pass match parameters
into `ProjectBase`, neither of which this step adds.

### Cost

One load per project (3–5 s), one extraction per case (0.09–0.18 s) and the
impact analysis once per case and setting (0.2 ms on `pipeline`, 1.8 ms on
`shopfront`). Because the impact analysis is an order of magnitude cheaper than
extraction, a grid is affordable: the round-1 results, 39 settings, a release
harness on 16 worker threads — `shopfront`'s 1 333 cases in about 4 minutes,
`pipeline`'s 148 in 16 s, and `macrokit`'s 385 as the held-out confirmation in
another 1.5 minutes, 5.5 minutes in all. The case queue is fed by a single
extractor thread and drained by the workers, because one `ProjectBase` owns one
in-memory file system; the workers only ever touch extracted graphs, so no
workspace is shared and the result does not depend on the thread count.

## What excludes a case

Every exclusion is counted and reported by reason:

- `cargo-failed` — Cargo could not run or failed without a diagnostic (a broken
  manifest, a missing toolchain).
- `timeout` — the run exceeded its limit (10 min for a check, 5 min for one test
  run).
- `blastr-failed` — Blastr returned an error for the case.
- `does-not-parse` / `does-not-compile` — a mutant that is not valid Rust, or
  that the compiler rejects, says nothing about which tests a change reaches.
- `stopped-at-name-resolution` — every error is a name-resolution error
  (`E0425`, `E0432`, `E0603`, …). rustc replaces an unresolved or private name
  with an error type and suppresses the errors the code around it would produce,
  so the error list is not the whole truth for that edit; the case is not
  scored.
- `crate-not-checked` — no target holding an item of the case was built, because
  a dependency failed to compile.
- `failed-only-in-doc-tests` — version 1 of Blastr does not analyse doctests
  (a documented limit), so a mutant that only breaks a doc example is not an
  accuracy question.

Items in targets the compiler never started are not counted as misses or as
false alarms either: they are counted as **unconfirmed** and left out of both
sides. So are the items of a target that reported a privacy error: rustc stops
checking privacy after the first one, so its error list cannot refute the other
sites Blastr lists in that target (see [Known limits](#known-limits)). A target
counts as judged when Cargo reports an artifact for it or a
diagnostic from it — Cargo labels every diagnostic with the crate root of the
target it came from. That happens whenever an edit to a library makes its
dependents fail to build: the dependents, the integration tests and the binary
that link against the library are then never compiled, and nothing can confirm
or refute what Blastr says about them.

## Known limits

- A Cargo project that sits inside the directory of another Cargo workspace is
  loaded with a crate missing from its graph: `testdata/tiny` lives inside the
  harness's own workspace, and its second crate disappears when it is loaded
  from there. The harness's own tests copy the project out of that tree first,
  and so should anyone analysing it by hand. This looks like a bug in the loading
  path rather than a property of the project; it is recorded here instead of
  being papered over.
- A target that failed with a message of its own but no source span (a
  `failure-note`) is not distinguished from one that never started, since Cargo
  names no crate root for such a message.
- Test names are compared as libtest prints them (the path without the crate
  segment, which is what `SelectedTest::filter` holds). Two tests with the same
  name in different targets cannot be told apart, which can only make Blastr
  look better than it is.
- Only `cargo check`'s and `cargo test`'s output is used; a build script or a
  procedural macro that fails shows up as `cargo-failed`.
- rustc reports at most one privacy error per target when a private field is
  *accessed* (`E0616`) and then stops checking the privacy of the other sites in
  that target, so a case whose errors include `E0616` can under-report the
  literals and patterns that name the field. Blastr may list a site the error
  list does not mention; the list is right and the compiler stopped early. The
  check counts such an item as **unconfirmed**, not as a false alarm: an item
  whose target already reported a privacy error (`E0451`, `E0603`, `E0616`,
  `E0624`) cannot be refuted by that target's error list. Measured on the sample
  projects: narrowing a field of `StageSpec` made `config_dsl` report the access
  at `config_dsl.rs:56` and nothing about the three literals in the same file
  that also name the field; removing that one error made the next literal
  (`:63`) report. The same holds for `storage::text::encode`, `decode_customer`
  and `decode_order` after `Address::line1` was narrowed.
- The `retype-field` edit wraps the type in a one-element tuple, and a tuple
  keeps `Debug`, `Ord`, `Copy` and the other traits the value may have had, so a
  reader that only formats, compares or copies the value still compiles, and so
  does a literal that passes the field through (`Money { currency:
  self.currency }`) because the value is already the new type. Blastr's C7 rule
  reaches every reader and every literal naming the field, so those sites are
  counted as false alarms it cannot avoid without type information. Measured on
  the sample projects: `Money::percent_bp`, `Money::scaled`, `product::cheapest`
  and the two `describe_impl` sites of `macrokit-derive`, all of which the
  compiler accepts after `Money::currency` or `FieldModel::name` was retyped.
- Removing an inherent method that shares its name with a provided trait method
  leaves the call sites compiling: they resolve to the trait's default instead,
  which the head graph already contains, so nothing is unresolved and nothing
  reports an error. Blastr's C5 rule reads every referrer on the base graph, so
  it lists them. Measured on `macrokit`: removing `ThermalProfile::name` leaves
  `profile.name()` resolving to `macrokit::describe::Describe::name`. Telling the
  two apart would need the resolution of the same call on the head side.
- Items that a macro produced have the span of the macro call, not of a syntax
  node of their own, so no edit is planned for them. The number of unplanned
  edits is reported.
- An item's distance is the length of its strongest evidence path, and the
  search follows an item's own references once, from that path. When the
  strongest path is also the longest, its callers are one step past it and the
  depth limit cuts them although a shorter path to the item exists. Measured on
  `macrokit` at depth 8, decay 0.8: `session::station` is listed at distance 8
  through a fully resolved chain, so
  `station_commands_coexist_with_the_library_commands` is not selected even
  though a candidate chain reaches it at distance 6 (test cases `358`, `417`).
  Following an item again when a shorter path arrives
  would carry the propagation on, but it changes the reachable set of every
  project and the default parameters cannot exercise it: at decay 0.6 a
  resolved hop is cheaper than a candidate one, so the longest path is never
  the strongest. Chains of nine steps on the shortest route are cut by the same
  depth limit without any shorter path to blame (cases `251`, `259`, `357`,
  `379`, `390`, `429`).
- Two candidate steps on one path compound below the score floor: on `macrokit`,
  `Entry::source` and `Issue::render` reach their failing tests at 0.042 against
  a floor of 0.05 (cases `168`, `395`).
- Behaviour propagation continues from a function's references, not from a
  type's. A changed procedural macro reaches the items whose expansion names it
  and, through their generated methods, the code that calls those methods; code
  that only *mentions* the annotated type does not continue the propagation.
  Measured on `macrokit`: four tests reach the changed `parse_describe_attribute`
  only through a `uses-type` edge into `CommandSpec` or `Estimate` (case `011`),
  while the call chain the rules can follow is longer than the depth limit.
- The compiler check maps an error to the innermost project symbol containing
  its span. An error inside the body of a `macro_rules!` macro of the project,
  or inside an impl a `#[derive]` generated, therefore maps to the macro-side
  symbol: the macro definition, or the generated impl at the attribute. Blastr
  deliberately never lists a macro-generated item as *needs editing* (it has no
  text of its own) and lists the invokers of a changed macro, not the macro
  definition of a changed signature of an invoker, so such errors are counted
  as misses. Measured on `macrokit` with the complete graph: 46 of the 87
  compiler misses name `macros::ensure` or a derive-generated `Describe` impl.
