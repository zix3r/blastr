# pipeline

A small record-processing pipeline for log lines: stages are plugged together
from a one-line textual spec, records flow through them, and the run is
reported with per-stage counts and summaries. The workspace pretends to be the
core of an in-house log/ETL tool: parse a log document, filter noise, normalise
severity spellings, redact secrets, group and enrich records, then truncate and
report.

```text
$ cargo run -q -p pipeline-app -- "level|filter:no-debug|dedup|redact|group:level|enrich|truncate:28"
pipeline level>filter>dedup>redact>group>enrich>truncate
8 in -> 6 out across 7 stage(s)
  normalize-level: 8 -> 8
  ...
```

## Crates

| crate | role |
| --- | --- |
| `pipeline-core` | `Record`, `Level`, `PipelineError`, the `Stage`/`Sink`/`Describe` traits, `Chain`, the runner, the log reader, sinks |
| `pipeline-stages` | concrete stages (`Map`, `Filter`, `Dedup`, `Redact`, `Truncate`, `Group`, ...), predicates/transforms, `TextSink`, the `Partition` trait |
| `pipeline-app` | the spec language (`StageSpec`, `build_pipeline`), the app-level `Validate` stage, the report renderer and the demo binary |

Code: 2 991 lines across `src/` (not counting tests); tests: 2 580 lines,
137 tests, whole suite runs in ~0.3 s. No dependencies outside `std`.

## Log and spec formats

Log line: `LEVEL source message words... key=value...` (blank lines and `#`
comments are skipped). Spec: `kind` or `kind:argument` tokens separated by `|`:

| spec | stage |
| --- | --- |
| `level` | `NormalizeLevel` |
| `filter:error`, `filter:warn`, `filter:no-debug`, `filter:health`, `filter:request` | `Filter` with a named predicate |
| `map:upper`, `map:lower`, `map:trim`, `map:length`, `map:words`, `map:severity` | `Map` with a named transform |
| `tag:env=prod` | `Tag` |
| `truncate:40` / `truncate:40:false` | `Truncate` (ellipsis on/off) |
| `dedup`, `redact`, `tap`, `enrich`, `limit:5`, `group:source`, `group:level`, `validate:status,request_id` | the matching stage |

## Blastr rule coverage

| rule | where |
| --- | --- |
| required trait methods | `Stage::name/kind/apply/summarise` (`pipeline-core/src/stage.rs`), `Sink::label/written/emit` (`pipeline-core/src/sink.rs`), `Partition::key` (`pipeline-stages/src/partition.rs`) |
| provided (default) methods | `Stage::apply_all`, `Stage::describe` (`pipeline-core/src/stage.rs`), `Sink::flush`, `Sink::capacity` (`pipeline-core/src/sink.rs`), `Partition::same_bucket/describe` (`pipeline-stages/src/partition.rs`), `Describe::headline/mentions` (`pipeline-core/src/describe.rs`) |
| impl overrides a default | `Dedup::apply_all` (`pipeline-stages/src/dedup.rs`), `Truncate::describe` (`truncate.rs`), `Tap::describe` (`tap.rs`), `Chain::describe` (`pipeline-core/src/stage.rs`), `Validate::describe` (`pipeline-app/src/validate.rs`), `CollectSink::flush` / `TextSink::flush`, `TextSink::capacity`, `TallySink::capacity` |
| impl keeps the default | every other stage: `Map::apply_all`, `Filter::apply_all`, `Tag`, `Redact`, `NormalizeLevel`, `Enrich`, `Limit`, `Group`, `Chain::apply_all`; `CountingSink` uses the default `flush` |
| trait objects | `Pipeline.stages: Vec<Box<dyn Stage>>` and `Pipeline::run_into(..., &mut dyn Sink)` (`pipeline-core/src/pipeline.rs`), `describe_sink(&dyn Sink)` (`pipeline-core/src/sink.rs`), `build_stage -> Box<dyn Stage>` (`pipeline-app/src/config.rs`), `&mut dyn Stage` in `pipeline-stages/tests/dyn_dispatch.rs` |
| generic functions + `where` | `run_stage<S: Stage + ?Sized>`, `run_pair<A, B>` (`pipeline-core/src/pipeline.rs`), `drain<I: IntoIterator, S: Sink + ?Sized>`, `RecordBuilder::fields<I, K, V>` (`record.rs`), `Group<P: Partition>` (`pipeline-stages/src/group.rs`), `Validate::new<I, S>` (`pipeline-app/src/validate.rs`) |
| blanket impl `impl<T: Display> Describe for T` | `pipeline-core/src/describe.rs` |
| extension trait | `StageExt` blanket over `T: Stage + Sized` with `then`, `boxed`, `apply_each` (`pipeline-core/src/stage.rs`) |
| same trait implemented across crates | `Stage` implemented in `pipeline-core` (`Chain`), `pipeline-stages` (eleven stages), `pipeline-app` (`Validate`); `Sink` implemented in `pipeline-core` (`CollectSink`, `CountingSink`, `TallySink`) and `pipeline-stages` (`TextSink`) |
| closures / functions as values | `Map { transform: Box<dyn FnMut(Record) -> Record> }`, `Filter { keep: Box<dyn Fn(&Record) -> bool> }`; `Map::from_fn(..., uppercase_message)`, `Filter::from_fn(..., is_error)` (`pipeline-stages/src/transforms.rs`, `predicates.rs`, `pipeline-app/src/config.rs`) |
| iterator adaptors | `Pipeline::summaries`/`RunStats::detail`, `Redact::scrub_message`, `Group::summarise`, `Record::fields_iter().map(...)` (`redact.rs`, `group.rs`, `record.rs`) |
| custom `Iterator` impls | `Fields`, `Words` (`pipeline-core/src/record.rs`), `LineReader` yielding `Result<Record, PipelineError>` (`pipeline-core/src/reader.rs`) |
| error enum + `From` for `?` | `PipelineError` with `From<ParseIntError>` and `From<ParseBoolError>` (`pipeline-core/src/error.rs`), used with `?` in `StageSpec::number_and_flag` and `StageSpec::number` (`pipeline-app/src/config.rs`), `Level::from_label(...)?` in `NormalizeLevel` (`normalize.rs`) |
| associated type + associated const | `Partition::LABEL` and `Partition::Key` on `BySource`/`ByLevel`/`ByField` (`pipeline-stages/src/partition.rs`), used by `Group<P>` as `BTreeMap<P::Key, usize>` |
| exhaustive `match` (variant added/removed) | `Level::as_str`, `Level::badge`, `Level::severity` (`pipeline-core/src/level.rs`), `StageKind::as_str` (`pipeline-core/src/stage.rs`) |
| struct fields (add/retype/remove) | `Record` (private `source`, `level`, `message`, `fields`), `RecordBuilder`, `StageReport`, `RunStats`/`StageStats`, `Report` |
| builders | `RecordBuilder` (`pipeline-core/src/record.rs`), `Truncate::with_ellipsis`, `Redact::with_mask/with_keys`, `Enrich::with_services`, `Tag::keep_existing`, `Pipeline::with_stage` |
| visibility | `Pipeline::push_boxed` is `pub` for `pipeline-app`; `Record::set_field`/`remove_field` are `pub` for stages; `Fields::new`/`Words::new` are private helpers |

## Tests

| target | tests | kind |
| --- | --- | --- |
| `pipeline-core` unit (`src/*.rs`) | 40 | unit |
| `pipeline-core/tests/runner_api.rs` | 6 | integration: runner, sinks, chain, generic helpers |
| `pipeline-stages` unit | 54 | unit per stage/predicate/transform |
| `pipeline-stages/tests/stage_pipeline.rs` | 8 | integration: whole pipelines, exact rendered lines and summaries |
| `pipeline-stages/tests/dyn_dispatch.rs` | 5 | only `&mut dyn Stage` / `Vec<Box<dyn Stage>>`, default vs overridden batching |
| `pipeline-app` unit | 13 | unit: spec parsing, report rendering, `Validate` |
| `pipeline-app/tests/full_pipeline.rs` | 5 | integration: `DEFAULT_SPEC` over `SAMPLE_LOG`, exact report text |
| `pipeline-app/tests/config_dsl.rs` | 6 | integration: spec language, configuration errors, ellipsis flag |

No doctests (code fences in docs are marked `text`), no filesystem or network
access, no sleeps or randomness.

## Commands

```sh
cargo build --all-targets
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo run -p pipeline-app            # uses DEFAULT_SPEC and SAMPLE_LOG
cargo run -p pipeline-app -- "tag:env=prod|filter:error|truncate:24:false"
```
