# Blastr validation report

Blastr 0.1.1 (build `0.1.1+59395bb3`), rust-analyzer 0.0.358.
Parameters: depth 8, decay 0.8, candidate weight 0.5, adaptation weight 1, behaviour weight 0.8, score floor 0.05, minimum confidence possible, ignore [].

`macrokit` compiler: 6 edit(s) could not be planned (no syntax of their own, or the edit does not apply).

`macrokit` tests: 46 edit(s) could not be planned (no syntax of their own, or the edit does not apply).

`pipeline` compiler: 4 edit(s) could not be planned (no syntax of their own, or the edit does not apply).

`pipeline` tests: 2 edit(s) could not be planned (no syntax of their own, or the edit does not apply).

`shopfront` compiler: 8 edit(s) could not be planned (no syntax of their own, or the edit does not apply).

`shopfront` tests: 2 edit(s) could not be planned (no syntax of their own, or the edit does not apply).

## Compiler check: *needs editing* items

Every case applies one seeded edit to one definition, runs Blastr on the original and the edited project, and runs `cargo check --all-targets` on the edited copy. The symbols with compiler errors (and the changed symbol itself) are what Blastr should have listed as *needs editing*.

| project | revision | crates | symbols | cases | excluded | misses | false alarms | expected | listed | unconfirmed | blastr items | reachable | listed / reachable |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| macrokit | `5766186` | 9 | 1632 | 2534 | 109 | 5 | 36 | 5550 | 5581 | 871 | 109897 | 152912 | 72% |
| pipeline | `5766186` | 9 | 1006 | 1589 | 48 | 2 | 5 | 2633 | 2637 | 1373 | 35266 | 41566 | 85% |
| shopfront | `5766186` | 10 | 811 | 1290 | 86 | 0 | 3 | 2679 | 2697 | 1660 | 32115 | 39113 | 82% |

### Per change kind

| project | kind | cases | excluded | misses | false alarms | expected | listed | unconfirmed | blastr items | reachable | listed / reachable |
|---|---|---|---|---|---|---|---|---|---|---|---|
| macrokit | add-field | 34 | 0 | 0 | 0 | 47 | 47 | 0 | 1897 | 3921 | 48% |
| macrokit | add-parameter | 603 | 0 | 0 | 0 | 1301 | 1301 | 200 | 21564 | 27667 | 78% |
| macrokit | add-required-trait-method | 1 | 0 | 0 | 0 | 0 | 0 | 10 | 0 | 8 | 0% |
| macrokit | add-variant | 15 | 0 | 0 | 0 | 22 | 22 | 0 | 1652 | 2388 | 69% |
| macrokit | change-return-type | 428 | 0 | 0 | 4 | 1165 | 1169 | 339 | 21097 | 27215 | 78% |
| macrokit | narrow-visibility | 437 | 8 | 0 | 0 | 233 | 233 | 63 | 1244 | 20361 | 6% |
| macrokit | remove-field | 180 | 0 | 0 | 0 | 644 | 644 | 24 | 19027 | 17730 | 107% |
| macrokit | remove-function | 576 | 101 | 5 | 8 | 1103 | 1106 | 178 | 12819 | 18581 | 69% |
| macrokit | remove-variant | 80 | 0 | 0 | 0 | 415 | 415 | 33 | 11603 | 12976 | 89% |
| macrokit | retype-field | 180 | 0 | 0 | 24 | 620 | 644 | 24 | 18994 | 22065 | 86% |
| pipeline | add-field | 31 | 0 | 0 | 0 | 25 | 25 | 2 | 798 | 1009 | 79% |
| pipeline | add-parameter | 467 | 0 | 0 | 0 | 617 | 617 | 289 | 8413 | 9108 | 92% |
| pipeline | add-required-trait-method | 5 | 0 | 0 | 0 | 13 | 13 | 16 | 29 | 147 | 20% |
| pipeline | add-variant | 3 | 0 | 0 | 0 | 7 | 7 | 0 | 186 | 487 | 38% |
| pipeline | change-return-type | 337 | 0 | 0 | 4 | 526 | 530 | 545 | 8543 | 9255 | 92% |
| pipeline | narrow-visibility | 237 | 2 | 0 | 0 | 316 | 317 | 100 | 2223 | 6055 | 37% |
| pipeline | remove-field | 81 | 0 | 2 | 0 | 279 | 277 | 58 | 5032 | 3251 | 155% |
| pipeline | remove-function | 332 | 46 | 0 | 0 | 464 | 464 | 215 | 3656 | 5638 | 65% |
| pipeline | remove-variant | 16 | 0 | 0 | 0 | 124 | 124 | 94 | 1407 | 1477 | 95% |
| pipeline | retype-field | 80 | 0 | 0 | 1 | 262 | 263 | 54 | 4979 | 5139 | 97% |
| shopfront | add-field | 14 | 0 | 0 | 0 | 19 | 19 | 7 | 771 | 975 | 79% |
| shopfront | add-parameter | 301 | 0 | 0 | 0 | 563 | 563 | 157 | 4767 | 4767 | 100% |
| shopfront | add-variant | 9 | 0 | 0 | 0 | 19 | 19 | 3 | 603 | 931 | 65% |
| shopfront | change-return-type | 208 | 0 | 0 | 0 | 460 | 460 | 297 | 4923 | 4923 | 100% |
| shopfront | narrow-visibility | 240 | 4 | 0 | 0 | 321 | 336 | 191 | 3283 | 9742 | 34% |
| shopfront | remove-field | 86 | 0 | 0 | 0 | 330 | 330 | 319 | 5319 | 5052 | 105% |
| shopfront | remove-function | 294 | 82 | 0 | 0 | 376 | 376 | 159 | 3057 | 3323 | 92% |
| shopfront | remove-variant | 52 | 0 | 0 | 0 | 264 | 264 | 208 | 4073 | 4075 | 100% |
| shopfront | retype-field | 86 | 0 | 0 | 3 | 327 | 330 | 319 | 5319 | 5325 | 100% |

## Test check: *may behave differently* items and test selection

Every case seeds one mutation into one function body, keeps it only if it compiles, and runs `cargo test --workspace --no-fail-fast`. Every test that then fails must be among the tests Blastr selects for the same change.

| project | revision | mutants | excluded | misses | selected | suite | selection |
|---|---|---|---|---|---|---|---|
| macrokit | `5766186` | 437 | 202 | 21 | 6270 | 38070 | 16% |
| pipeline | `5766186` | 332 | 132 | 0 | 3929 | 27400 | 14% |
| shopfront | `5766186` | 211 | 82 | 0 | 1825 | 12384 | 15% |

### Per mutation kind

| project | kind | cases | excluded | misses | selected | suite | selection |
|---|---|---|---|---|---|---|---|
| macrokit | default-value | 324 | 192 | 10 | 2981 | 21384 | 14% |
| macrokit | delete-call | 7 | 0 | 0 | 232 | 1134 | 20% |
| macrokit | negate-condition | 15 | 2 | 0 | 532 | 2106 | 25% |
| macrokit | swap-operator | 91 | 8 | 11 | 2525 | 13446 | 19% |
| pipeline | default-value | 303 | 132 | 0 | 2769 | 23427 | 12% |
| pipeline | delete-call | 1 | 0 | 0 | 101 | 137 | 74% |
| pipeline | negate-condition | 6 | 0 | 0 | 176 | 822 | 21% |
| pipeline | swap-operator | 22 | 0 | 0 | 883 | 3014 | 29% |
| shopfront | default-value | 161 | 82 | 0 | 971 | 7584 | 13% |
| shopfront | delete-call | 1 | 0 | 0 | 2 | 96 | 2% |
| shopfront | negate-condition | 6 | 0 | 0 | 92 | 576 | 16% |
| shopfront | swap-operator | 43 | 0 | 0 | 760 | 4128 | 18% |

## Performance

| project | check | load | blastr per case | cargo per case | warm check | baseline tests |
|---|---|---|---|---|---|---|
| macrokit | compiler | 5070 ms | 389 ms | 276 ms | 44 ms | 0 ms |
| macrokit | tests | 4243 ms | 626 ms | 2087 ms | 0 ms | 225 ms |
| pipeline | compiler | 4044 ms | 261 ms | 192 ms | 43 ms | 0 ms |
| pipeline | tests | 3769 ms | 396 ms | 2068 ms | 0 ms | 225 ms |
| shopfront | compiler | 3617 ms | 247 ms | 189 ms | 44 ms | 0 ms |
| shopfront | tests | 3693 ms | 407 ms | 2116 ms | 0 ms | 266 ms |

## Misses

- `macrokit` / `003-remove-function-macrokit_derive-derive_describe` / macrokit_derive::derive_describe — missed macrokit::commands::CommandSpec::handler, macrokit::commands::describe, macrokit::commands::tests::command_specs_describe_themselves, macrokit::spec::tests::description_of_a_spec_lists_its_fields, macrokit::value::tests::ranges_describe_their_bounds
- `macrokit` / `011-swap-operator-macrokit_derive-parse_describe_attribute` / macrokit_derive::parse_describe_attribute — missed a_commissioning_session_derives_the_estimate, a_commissioning_session_runs_end_to_end, commands::tests::describe_lists_schema_details, station::tests::station_commands_read_the_engine
- `macrokit` / `251-swap-operator-macrokit-quantity-Quantity-combine` / macrokit::quantity::Quantity::combine — missed station::tests::station_commands_read_the_engine
- `macrokit` / `259-swap-operator-macrokit-quantity-split_number` / macrokit::quantity::split_number — missed inconsistent_configurations_trip_the_station, station_commands_coexist_with_the_library_commands, the_report_combines_every_section
- `macrokit` / `357-default-value-macrokit-unit-UnitRegistry-lookup` / macrokit::unit::UnitRegistry::lookup — missed commands::tests::command_specs_describe_themselves, descriptions_render_for_command_output, engine::tests::baseline_report_is_a_table, engine::tests::defaults_are_applied_on_construction, profile::tests::dimensions_are_part_of_the_profile, profile::tests::products_check_their_dimension, profile::tests::the_baseline_profile_is_read_from_the_engine, profile::tests::the_profile_describes_itself, validation::tests::baseline_rules_pass_on_defaults
- `macrokit` / `379-default-value-macrokit-validation-Severity-as_str` / macrokit::validation::Severity::as_str — missed commands::tests::validate_reports_issues
- `macrokit` / `390-swap-operator-macrokit-validation-compare_values` / macrokit::validation::compare_values — missed a_commissioning_session_runs_end_to_end, commands::tests::validate_reports_issues
- `macrokit` / `429-swap-operator-macrokit-value-Range-contains` / macrokit::value::Range::contains — missed inconsistent_configurations_trip_the_station
- `pipeline` / `840-remove-field-pipeline_core-stage-Chain-first` / pipeline_core::stage::Chain::first — missed pipeline_core::stage::tests::chain_runs_both_halves
- `pipeline` / `842-remove-field-pipeline_core-stage-Chain-second` / pipeline_core::stage::Chain::second — missed pipeline_core::stage::tests::chain_runs_both_halves

## Exclusions

- stopped-at-name-resolution (compiler check): 243
- does-not-compile (tests check): 416

## How to read this

A **case** is one edited definition. It is *scored* unless the compiler or Cargo could not judge it; the reason is counted under Exclusions. **expected** counts the symbols the compiler reported errors in (or the tests that failed), **listed** counts what Blastr listed as needing editing (or the tests it selected) for the same edit. **unconfirmed** counts items the compiler could not judge: those in a crate it did not build, and those in a target where it stopped after the first privacy error. **blastr items** counts every item of the result, of both classes, and **reachable** what plain reverse reachability from the change returns; the last column is the first as a share of the second, so 100% means Blastr lists as much as a reference search, less means it is more selective.
