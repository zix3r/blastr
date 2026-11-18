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

# both checks over every project under validation/projects, then the report
CARGO_TARGET_DIR=../../target cargo run --release -- report
```

`report` writes `validation/results/<project>-<check>.json` and
`validation/results/report.md`; `--cached` reuses the JSON files instead of
running the checks again, `--projects a,b` picks the projects, `--limit` and
`--seed` take a sample, and `--help` prints the options. The last full run over
the three sample projects is committed as `validation/results/report.md`, next to
the per-case JSON it was rendered from.

Every case result records the file, the item, the edit kind and the text of the
item before and after the edit, so a miss can be reproduced and inspected with
`--case <id>`. Case ids are `NNN-<kind>-<symbol>`, numbered in the order the
graph lists the definitions, so two runs of the same project plan the same cases.

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
- `crate-not-checked` — no crate holding an item of the case was built, because
  a dependency failed to compile.
- `failed-only-in-doc-tests` — version 1 of Blastr does not analyse doctests
  (a documented limit), so a mutant that only breaks a doc example is not an
  accuracy question.

Items in crates that the compiler did not build are not counted as misses or as
false alarms either: they are counted as **unconfirmed** and left out of both
sides. That happens whenever an edit to a library makes its dependents fail to
build — the dependents are then never checked, and nothing can confirm or refute
what Blastr says about them.

## Known limits

- A Cargo project that sits inside the directory of another Cargo workspace is
  loaded with a crate missing from its graph: `testdata/tiny` lives inside the
  harness's own workspace, and its second crate disappears when it is loaded
  from there. The harness's own tests copy the project out of that tree first,
  and so should anyone analysing it by hand. This looks like a bug in the loading
  path rather than a property of the project; it is recorded here instead of
  being papered over.
- A crate is treated as checked or unchecked as a whole. Cargo reports its
  progress per package, not per target, so a case where only one target of a
  package was built is not distinguished.
- Test names are compared as libtest prints them (the path without the crate
  segment, which is what `SelectedTest::filter` holds). Two tests with the same
  name in different targets cannot be told apart, which can only make Blastr
  look better than it is.
- Only `cargo check`'s and `cargo test`'s output is used; a build script or a
  procedural macro that fails shows up as `cargo-failed`.
- Items that a macro produced have the span of the macro call, not of a syntax
  node of their own, so no edit is planned for them. The number of unplanned
  edits is reported.
