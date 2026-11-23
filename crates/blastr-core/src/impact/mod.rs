//! Impact analysis: from the structural changes between two graphs to the
//! items they can affect, ranked, with the evidence for each.

mod propagate;
mod rules;
mod select;
pub mod whatif;

use blastr_model::{
    AnalysisParams, CancelToken, Cancelled, Change, ImpactItem, ImpactLimits, RevisionGraph,
    TestSelection,
};

use crate::config::IgnoreSet;

/// Everything an impact analysis reads.
#[derive(Debug, Clone, Copy)]
pub struct ImpactInput<'a> {
    /// The graph of the older side.
    pub base: &'a RevisionGraph,
    /// The graph of the newer side.
    pub head: &'a RevisionGraph,
    /// The structural changes between the two.
    pub changes: &'a [Change],
    /// Depth, weights and filters.
    pub params: &'a AnalysisParams,
    /// Files whose items are never listed.
    pub ignore: &'a IgnoreSet,
}

/// What an impact analysis produces.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactOutput {
    /// The affected items, best first.
    pub items: Vec<ImpactItem>,
    /// The affected tests.
    pub tests: TestSelection,
    /// What the result leaves out.
    pub limits: ImpactLimits,
}

/// The items the changes reach, before tests are selected.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Propagation {
    /// The affected items, best first.
    pub items: Vec<ImpactItem>,
    /// What the result leaves out.
    pub limits: ImpactLimits,
}

/// Computes what `input.changes` can affect.
pub fn analyze(input: &ImpactInput<'_>, cancel: &CancelToken) -> Result<ImpactOutput, Cancelled> {
    let Propagation { items, limits } = propagate::propagate(input, cancel)?;
    cancel.check()?;
    let tests = select::select_tests(input.head, input.changes, &items);
    Ok(ImpactOutput {
        items,
        tests,
        limits,
    })
}
