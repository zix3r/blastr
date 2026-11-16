//! Running the phases of an extraction on several threads.
//!
//! rust-analyzer's database can be read from several threads at once. A clone of
//! [`RootDatabase`] shares the analysis state with the original, so every worker
//! gets a clone of its own and no thread writes to the database while the others
//! read from it.
//!
//! The work of an extraction has a shape: the definition map of a crate needs
//! the maps of its dependencies, the walk of a member crate needs its own map,
//! and the inactive ranges of a module need the maps of its crate. The workers
//! therefore share one queue of work:
//!
//! - the definition maps come first, because everything waits for them,
//! - the inactive ranges come last, because reading them is the most expensive
//!   query of the analysis and only the modules of the members need them,
//! - the walk of a member crate stays on the calling thread, because the syntax
//!   trees a walk builds cannot move to another thread; it hands the modules it
//!   found to the workers.
//!
//! The walk of a crate starts as soon as its definition map is done, so the
//! workers are already reading inactive ranges while the dependency chain of the
//! project is still being resolved.

use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use blastr_model::{CancelToken, Location, Progress};
use ra_ap_hir::{AnyDiagnostic, Crate, Module, attach_db, db::HirDatabase};
use ra_ap_ide::RootDatabase;

use crate::locate::{Locator, Paths};
use crate::{Error, check_cancel};

/// How long a waiting thread sleeps before it looks for work again.
const PAUSE: Duration = Duration::from_millis(10);

/// The workers an extraction runs on.
pub(crate) struct Threads {
    /// The database the project was loaded into; every worker clones it.
    database: RootDatabase,
    /// The number of workers.
    count: usize,
}

impl Threads {
    /// Prepares `requested` workers, or as many as the machine has cores when
    /// the caller has no preference.
    pub(crate) fn new(database: &RootDatabase, requested: Option<usize>) -> Threads {
        let available = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        let count = requested.filter(|count| *count > 0).unwrap_or(available);
        Threads {
            database: database.clone(),
            count,
        }
    }

    /// The number of workers.
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// A clone of the database for one worker.
    fn database(&self) -> RootDatabase {
        self.database.clone()
    }
}

/// The crates of the project, and which of them the member crates are.
pub(crate) struct Crates {
    /// Every crate of the crate graph.
    crates: Vec<Crate>,
    /// The member each crate is the root of, if it is a member root.
    members: HashMap<Crate, usize>,
    /// The number of members.
    member_count: usize,
}

impl Crates {
    /// Records the crate graph of the project and which crates the members are.
    pub(crate) fn new(
        db: &dyn HirDatabase,
        members: &[(Crate, usize)],
        member_count: usize,
    ) -> Crates {
        let crates = Crate::all(db);
        let mut member_of = HashMap::new();
        for &(krate, member) in members {
            member_of.insert(krate, member);
        }
        Crates {
            crates,
            members: member_of,
            member_count,
        }
    }
}

/// What the workers and the calling thread share while an extraction runs.
struct Shared {
    /// The definition maps that are ready to be computed.
    maps: VecDeque<usize>,
    /// The number of dependencies that are still outstanding, per crate.
    left: Vec<usize>,
    /// The crates that wait for a crate.
    dependents: Vec<Vec<usize>>,
    /// The number of definition maps that have not been computed yet.
    maps_left: usize,
    /// The member crates whose definition map is done.
    walks: VecDeque<usize>,
    /// The number of member crates that have not been walked yet.
    walks_left: usize,
    /// The modules whose inactive ranges are needed.
    modules: VecDeque<Module>,
    /// The number of inactive ranges that are queued or being read.
    modules_left: usize,
    /// The number of modules whose inactive ranges are needed.
    modules_total: usize,
    /// The inactive ranges found so far.
    inactive: Vec<Location>,
    /// Whether a query panicked, which ends the work without an answer.
    failed: bool,
}

impl Shared {
    /// Records the crate graph the work follows.
    fn new(db: &dyn HirDatabase, crates: &Crates) -> Shared {
        let count = crates.crates.len();
        let mut position: HashMap<Crate, usize> = HashMap::with_capacity(count);
        for (index, krate) in crates.crates.iter().enumerate() {
            position.insert(*krate, index);
        }
        let mut left = Vec::with_capacity(count);
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];
        for (index, krate) in crates.crates.iter().enumerate() {
            let dependencies: Vec<usize> = krate
                .dependencies(db)
                .iter()
                .filter_map(|dependency| position.get(&dependency.krate).copied())
                .collect();
            left.push(dependencies.len());
            for dependency in dependencies {
                dependents[dependency].push(index);
            }
        }
        let maps = (0..count).filter(|index| left[*index] == 0).collect();
        Shared {
            maps,
            left,
            dependents,
            maps_left: count,
            walks: VecDeque::new(),
            walks_left: crates.member_count,
            modules: VecDeque::new(),
            modules_left: 0,
            modules_total: 0,
            inactive: Vec::new(),
            failed: false,
        }
    }
}

/// The queue the workers share, with the condition variable that wakes them.
struct Wake {
    /// The shared state.
    state: Mutex<Shared>,
    /// Signals that work was added or that a worker finished its share.
    changed: Condvar,
}

impl Wake {
    /// Changes the shared state and wakes every worker.
    fn update(&self, change: impl FnOnce(&mut Shared)) {
        {
            let mut state = self.lock();
            change(&mut state);
        }
        self.changed.notify_all();
    }

    /// Locks the shared state, ignoring a panic of another worker.
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Waits for a change of the shared state.
    fn wait<'a>(&'a self, state: MutexGuard<'a, Shared>) -> MutexGuard<'a, Shared> {
        self.changed
            .wait_timeout(state, PAUSE)
            .unwrap_or_else(|error| error.into_inner())
            .0
    }
}

/// Everything one extraction hands to the workers.
pub(crate) struct Input<'a> {
    /// The database, attached to the calling thread.
    pub(crate) db: &'a dyn HirDatabase,
    /// The crates of the project.
    pub(crate) crates: &'a Crates,
    /// The workers.
    pub(crate) threads: &'a Threads,
    /// The paths of the files, for the locators of the workers.
    pub(crate) paths: Arc<Paths>,
    /// Asks the caller to stop.
    pub(crate) cancel: &'a CancelToken,
    /// Tells the caller how far the extraction is.
    pub(crate) progress: &'a dyn Progress,
}

/// Computes the definition maps, walks the member crates and reads the inactive
/// ranges, each on the first thread that is free.
///
/// `walk` runs on the calling thread and is handed the member index; it returns
/// the walk of that member and the modules whose inactive ranges are needed,
/// which `walked` receives.
pub(crate) fn run<T, F>(
    input: Input<'_>,
    walked: &mut [Option<T>],
    walk: F,
) -> Result<Vec<Location>, Error>
where
    F: Fn(usize) -> Result<(T, Vec<Module>), Error>,
{
    let shared = Wake {
        state: Mutex::new(Shared::new(input.db, input.crates)),
        changed: Condvar::new(),
    };
    std::thread::scope(|scope| {
        for index in 0..input.threads.len() {
            let database = input.threads.database();
            let paths = Arc::clone(&input.paths);
            let shared = &shared;
            // A thread that cannot be started leaves its share to the others,
            // which take every piece of work that becomes ready.
            _ = std::thread::Builder::new()
                .name(format!("blastr-extract-{index}"))
                .spawn_scoped(scope, move || {
                    let db: &dyn HirDatabase = &database;
                    attach_db(db, || work(db, input.crates, shared, &paths, input.cancel));
                });
        }
        walk_members(
            &shared,
            input.crates,
            walked,
            &walk,
            input.cancel,
            input.progress,
        )?;
        wait_for_diagnostics(&shared, input.cancel, input.progress)?;
        Ok::<(), Error>(())
    })?;
    check_cancel(input.cancel)?;
    Ok(shared
        .state
        .into_inner()
        .unwrap_or_else(|error| error.into_inner())
        .inactive)
}

/// Reads definition maps and inactive ranges until there is nothing left.
fn work(
    db: &dyn HirDatabase,
    crates: &Crates,
    shared: &Wake,
    paths: &Arc<Paths>,
    cancel: &CancelToken,
) {
    let mut locator: Option<Locator<'_>> = None;
    loop {
        let Some(task) = take_task(shared, cancel) else {
            return;
        };
        match task {
            Task::DefMap(index) => define(db, crates, shared, index),
            Task::Diagnose(module) => {
                let locator =
                    locator.get_or_insert_with(|| Locator::with_paths(db, Arc::clone(paths)));
                let ranges = inactive_ranges(db, module, locator);
                shared.update(|state| {
                    state.modules_left = state.modules_left.saturating_sub(1);
                    state.inactive.extend(ranges);
                });
            }
        }
    }
}

/// The work one worker takes from the queue.
enum Task {
    /// Compute the definition map of a crate.
    DefMap(usize),
    /// Read the inactive ranges of a module.
    Diagnose(Module),
}

/// Takes the next piece of work, or `None` when every piece is done.
fn take_task(shared: &Wake, cancel: &CancelToken) -> Option<Task> {
    let mut state = shared.lock();
    loop {
        if cancel.is_cancelled() || state.failed {
            return None;
        }
        if let Some(index) = state.maps.pop_front() {
            return Some(Task::DefMap(index));
        }
        if let Some(module) = state.modules.pop_front() {
            return Some(Task::Diagnose(module));
        }
        if state.maps_left == 0 && state.modules_left == 0 && state.walks_left == 0 {
            return None;
        }
        state = shared.wait(state);
    }
}

/// Computes one definition map and hands out the work it unlocks.
///
/// rust-analyzer uses unwinding to cancel a query and can panic on code it
/// cannot analyse, so a panic ends the work instead of leaving the workers
/// waiting for a crate that will never be done.
fn define(db: &dyn HirDatabase, crates: &Crates, shared: &Wake, index: usize) {
    let defined = catch_unwind(AssertUnwindSafe(|| {
        ra_ap_hir::crate_def_map(db, crates.crates[index].base());
    }));
    if defined.is_err() {
        shared.update(|state| state.failed = true);
        return;
    }
    shared.update(|state| {
        state.maps_left = state.maps_left.saturating_sub(1);
        if let Some(member) = crates.members.get(&crates.crates[index]) {
            state.walks.push_back(*member);
        }
        let dependents = std::mem::take(&mut state.dependents[index]);
        for dependent in dependents {
            state.left[dependent] = state.left[dependent].saturating_sub(1);
            if state.left[dependent] == 0 {
                state.maps.push_back(dependent);
            }
        }
    });
}

/// Walks every member crate that becomes ready, on the calling thread.
fn walk_members<T, F>(
    shared: &Wake,
    crates: &Crates,
    walked: &mut [Option<T>],
    walk: &F,
    cancel: &CancelToken,
    progress: &dyn Progress,
) -> Result<(), Error>
where
    F: Fn(usize) -> Result<(T, Vec<Module>), Error>,
{
    let total = crates.member_count as u64;
    let mut done = 0;
    loop {
        check_cancel(cancel)?;
        let member = {
            let mut state = shared.lock();
            loop {
                if state.failed {
                    return Err(Error::Internal(
                        "a definition map of the project could not be computed".to_owned(),
                    ));
                }
                if let Some(member) = state.walks.pop_front() {
                    break Some(member);
                }
                if state.maps_left == 0 {
                    break None;
                }
                state = shared.wait(state);
            }
        };
        progress.report("extract", done, Some(total));
        let Some(member) = member else {
            return Ok(());
        };
        let walked_member = catch_unwind(AssertUnwindSafe(|| walk(member)));
        let (result, modules) = match walked_member {
            Ok(walked) => walked?,
            Err(_panic) => {
                shared.update(|state| state.failed = true);
                return Err(Error::Internal(
                    "a crate of the project could not be walked".to_owned(),
                ));
            }
        };
        if let Some(slot) = walked.get_mut(member) {
            *slot = Some(result);
        }
        shared.update(|state| {
            state.walks_left = state.walks_left.saturating_sub(1);
            state.modules_left += modules.len();
            state.modules_total += modules.len();
            state.modules.extend(modules);
        });
        done += 1;
    }
}

/// Waits until every inactive range has been read.
fn wait_for_diagnostics(
    shared: &Wake,
    cancel: &CancelToken,
    progress: &dyn Progress,
) -> Result<(), Error> {
    loop {
        let (left, total) = {
            let state = shared.lock();
            if state.modules_left == 0 || cancel.is_cancelled() || state.failed {
                return Ok(());
            }
            (state.modules_left, state.modules_total)
        };
        progress.report("extract", (total - left) as u64, Some(total as u64));
        std::thread::sleep(PAUSE);
    }
}

/// The ranges of the code of `module` that the configuration disables.
///
/// rust-analyzer reports them among the diagnostics of the module, which also
/// validate every body of the module; that is the price of the answer, and the
/// caller runs it for several modules at once.
pub(crate) fn inactive_ranges(
    db: &dyn HirDatabase,
    module: Module,
    locator: &Locator<'_>,
) -> Vec<Location> {
    let mut diagnostics = Vec::new();
    let done = catch_unwind(AssertUnwindSafe(|| {
        module.diagnostics(db, &mut diagnostics, false);
    }));
    if done.is_err() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    for diagnostic in diagnostics {
        let AnyDiagnostic::InactiveCode(code) = diagnostic else {
            continue;
        };
        let range = code.node.map(|pointer| pointer.text_range());
        let location = catch_unwind(AssertUnwindSafe(|| {
            locator.location(range.file_id, range.value)
        }))
        .ok()
        .flatten();
        if let Some(location) = location {
            ranges.push(location);
        }
    }
    ranges
}
