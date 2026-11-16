//! Timings of one extraction.
//!
//! The extraction is the slow part of a run, so a caller can ask for a
//! [`ExtractReport`] that says how long each phase took and which files the
//! dependency pass spent its time on. Nothing is recorded unless the caller asks
//! for it, which keeps the recording out of the normal path.

use std::cell::Cell;
use std::time::{Duration, Instant};

use blastr_model::RelPath;

/// How long one file of the project took in the dependency pass.
#[derive(Debug, Clone)]
pub struct FileTiming {
    /// The file, relative to the repository root; `None` for code a macro produced.
    pub file: Option<RelPath>,
    /// The number of definitions whose references the pass looked at.
    pub containers: u64,
    /// The number of edges and unresolved references the file produced.
    pub references: u64,
    /// The time the pass spent on the file.
    pub seconds: f64,
}

/// How long one kind of work took.
#[derive(Debug, Clone)]
pub struct PhaseTiming {
    /// What the work is.
    pub name: &'static str,
    /// The number of times the work ran.
    pub calls: u64,
    /// The time the work took, including the work nested inside it.
    pub seconds: f64,
}

/// The timings of one extraction.
#[derive(Debug, Clone, Default)]
pub struct ExtractReport {
    /// The time spent on the definition maps, the walks of the member crates and
    /// the inactive ranges, which run on several threads at once.
    pub pipeline_pass: f64,
    /// The time spent collecting the dependency edges and the unresolved
    /// references, over all workers.
    pub edge_pass: f64,
    /// The time spent merging the results of the workers and sorting them.
    pub merge: f64,
    /// The per-file timings of the dependency pass, in no particular order.
    pub files: Vec<FileTiming>,
    /// The per-phase timings, in no particular order.
    pub phases: Vec<PhaseTiming>,
}

/// One kind of work the profiler keeps apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Reads the source of an item from the database.
    Source,
    /// Hashes the tokens of an item.
    Hashes,
    /// Lists the declarations of a module or the items of a block.
    Declarations,
    /// Reads the name, visibility and kind of one definition.
    ItemQuery,
    /// Walks one crate.
    CrateRoot,
    /// Walks one module.
    Module,
    /// Adds one definition.
    Symbol,
    /// Collects the inactive `cfg` ranges of a module.
    Diagnostics,
    /// Walks the items of a function body.
    Nested,
    /// Resolves a definition from syntax.
    ResolveDef,
    /// Maps a syntax node back to the real file.
    Site,
    /// Resolves a path.
    ResolvePath,
    /// Resolves a method call.
    ResolveMethod,
    /// Resolves a field expression.
    ResolveField,
    /// Resolves a record expression or pattern.
    ResolveRecord,
    /// Infers the type of an expression.
    InferType,
    /// Resolves the macro a call invokes.
    ResolveMacro,
    /// Expands a macro call.
    ExpandMacro,
    /// Parses or expands the file a region belongs to.
    Parse,
    /// Adds the `Implements` edges of the impl blocks.
    Implements,
    /// Adds the edges of a trait-method call to the workspace impls.
    Dispatch,
    /// Finds the definition an identifier pattern names.
    ResolveBinding,
    /// Reads the parts of a format string.
    FormatArgs,
    /// Walks the syntax of one region.
    Visit,
}

/// Every kind, in the order of the report.
pub(crate) const KINDS: [Kind; 24] = [
    Kind::Source,
    Kind::Hashes,
    Kind::Declarations,
    Kind::ItemQuery,
    Kind::CrateRoot,
    Kind::Module,
    Kind::Symbol,
    Kind::Diagnostics,
    Kind::Nested,
    Kind::ResolveDef,
    Kind::Site,
    Kind::ResolvePath,
    Kind::ResolveMethod,
    Kind::ResolveField,
    Kind::ResolveRecord,
    Kind::InferType,
    Kind::ResolveMacro,
    Kind::ExpandMacro,
    Kind::Parse,
    Kind::Implements,
    Kind::Dispatch,
    Kind::ResolveBinding,
    Kind::FormatArgs,
    Kind::Visit,
];

impl Kind {
    /// What the work is, for the report.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Source => "read item source",
            Kind::ItemQuery => "read symbol facts",
            Kind::CrateRoot => "walk crates",
            Kind::Module => "walk modules",
            Kind::Symbol => "add definitions",
            Kind::Hashes => "hash item tokens",
            Kind::Declarations => "list declarations",
            Kind::Diagnostics => "collect inactive ranges",
            Kind::Nested => "walk function bodies",
            Kind::ResolveDef => "resolve syntax to a definition",
            Kind::Site => "map syntax back to a location",
            Kind::ResolvePath => "resolve paths",
            Kind::ResolveMethod => "resolve method calls",
            Kind::ResolveField => "resolve field accesses",
            Kind::ResolveRecord => "resolve literals and patterns",
            Kind::InferType => "infer expression types",
            Kind::ResolveMacro => "resolve macro calls",
            Kind::ExpandMacro => "expand macro calls",
            Kind::Parse => "parse or expand files",
            Kind::Implements => "collect trait impls",
            Kind::Dispatch => "find trait dispatch candidates",
            Kind::ResolveBinding => "resolve identifier patterns",
            Kind::FormatArgs => "read format strings",
            Kind::Visit => "walk regions",
        }
    }

    /// The position of the kind in the report.
    fn index(self) -> usize {
        self as usize
    }
}

/// The time and the number of calls of one kind of work.
#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    /// The time the work took.
    duration: Duration,
    /// The number of times the work ran.
    calls: u64,
}

/// An entry with no time and no calls, for starting a profile.
const EMPTY: Entry = Entry {
    duration: Duration::ZERO,
    calls: 0,
};

/// Records the time spent in each kind of work.
///
/// Recording is off unless the caller asks for a report; [`Profile::start`]
/// then returns `None`, and [`Profile::finish`] does nothing. The times are
/// recorded through cells, so a `&Profile` is enough to record even inside a
/// method that only borrows the collector.
#[derive(Debug, Clone)]
pub(crate) struct Profile {
    /// Whether the times are recorded at all.
    enabled: bool,
    /// The time and the call count of every kind.
    entries: [Cell<Entry>; KINDS.len()],
}

impl Profile {
    /// A profile that records nothing, or everything when `enabled` is true.
    pub(crate) fn new(enabled: bool) -> Profile {
        Profile {
            enabled,
            entries: [const { Cell::new(EMPTY) }; KINDS.len()],
        }
    }

    /// Whether times are recorded.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Starts timing one kind of work, if times are recorded.
    pub(crate) fn start(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    /// Adds the time since `start` to `kind`.
    pub(crate) fn finish(&self, kind: Kind, start: Option<Instant>) {
        self.finish_with(kind, start, 1);
    }

    /// Adds the time since `start` to `kind`, with the number of calls it stands for.
    pub(crate) fn finish_with(&self, kind: Kind, start: Option<Instant>, calls: u64) {
        if let Some(start) = start {
            self.add(kind, start.elapsed(), calls);
        }
    }

    /// Adds `calls` calls that took `duration` to `kind`.
    pub(crate) fn add(&self, kind: Kind, duration: Duration, calls: u64) {
        if !self.enabled {
            return;
        }
        let entry = self.entries[kind.index()].get();
        self.entries[kind.index()].set(Entry {
            duration: entry.duration + duration,
            calls: entry.calls + calls,
        });
    }

    /// Adds the times of another profile, which ran on another thread.
    pub(crate) fn merge(&self, other: &Profile) {
        if !self.enabled || !other.enabled {
            return;
        }
        for (mine, theirs) in self.entries.iter().zip(other.entries.iter()) {
            let theirs = theirs.get();
            let mine_entry = mine.get();
            mine.set(Entry {
                duration: mine_entry.duration + theirs.duration,
                calls: mine_entry.calls + theirs.calls,
            });
        }
    }

    /// The phases of this profile, in no particular order.
    pub(crate) fn phases(&self) -> Vec<PhaseTiming> {
        if !self.enabled {
            return Vec::new();
        }
        KINDS
            .iter()
            .filter(|kind| self.entries[kind.index()].get().calls > 0)
            .map(|kind| {
                let entry = self.entries[kind.index()].get();
                PhaseTiming {
                    name: kind.name(),
                    calls: entry.calls,
                    seconds: entry.duration.as_secs_f64(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::KINDS;

    #[test]
    fn the_kinds_are_in_declaration_order() {
        for (index, kind) in KINDS.iter().enumerate() {
            assert_eq!(*kind as usize, index, "`{kind:?}` is out of place");
        }
    }
}
