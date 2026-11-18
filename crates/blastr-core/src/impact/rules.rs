//! The impact rule matrix: which change reaches which dependents, with what
//! class, certainty and reason.
//!
//! The matrix is data, not a nest of branches. Every [`Rule`] names the changes
//! it applies to, the graph it reads, the targets whose incoming edges it
//! follows, and one [`Source`] per group of edge kinds with its own filter,
//! certainty, reason and continuation. [`first_steps`] applies the table to one
//! change; [`onward`] is the behaviour-propagation step the search repeats from
//! every item it has reached.

use blastr_model::{
    Certainty, Change, ChangeDetail, ChangeKind, Delta, Edge, EdgeDetail, EdgeKind, GraphIndex,
    ImpactClass, Reason, RevisionGraph, SymbolId, SymbolKind, SymbolPath, Visibility,
};

/// Which side of the comparison a rule reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Side {
    /// The graph of the older side.
    Base,
    /// The graph of the newer side.
    Head,
}

/// The two graphs of a comparison and their lookup tables.
pub(crate) struct Graphs<'a> {
    /// The graph of the older side.
    pub(crate) base: &'a RevisionGraph,
    /// The graph of the newer side.
    pub(crate) head: &'a RevisionGraph,
    /// Lookup tables of the base graph.
    pub(crate) base_index: GraphIndex,
    /// Lookup tables of the head graph.
    pub(crate) head_index: GraphIndex,
}

impl<'a> Graphs<'a> {
    /// Builds the lookup tables of both sides.
    pub(crate) fn new(base: &'a RevisionGraph, head: &'a RevisionGraph) -> Graphs<'a> {
        Graphs {
            base,
            head,
            base_index: GraphIndex::new(base),
            head_index: GraphIndex::new(head),
        }
    }

    /// The graph of one side.
    pub(crate) fn graph(&self, side: Side) -> &'a RevisionGraph {
        match side {
            Side::Base => self.base,
            Side::Head => self.head,
        }
    }

    /// The lookup tables of one side.
    pub(crate) fn index(&self, side: Side) -> &GraphIndex {
        match side {
            Side::Base => &self.base_index,
            Side::Head => &self.head_index,
        }
    }
}

/// One dependency step a rule took: the edge it followed and what the reached
/// item means. The reached symbol is the edge's `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reach {
    /// The graph the edge index belongs to.
    pub(crate) side: Side,
    /// Index into that side's `edges`.
    pub(crate) edge: u32,
    /// The class the reached item gets when the path stops here.
    pub(crate) class: ImpactClass,
    /// How certain the step is.
    pub(crate) certainty: Certainty,
    /// Why the step applies.
    pub(crate) reason: Reason,
    /// Whether behaviour propagation continues from the reached item.
    pub(crate) continues: bool,
    /// Whether the rule cannot tell exactly who is affected.
    pub(crate) coarse: bool,
}

/// The changed symbol kinds a rule restricts itself to.
const ANY_KIND: &[SymbolKind] = &[];
/// Functions and methods.
const FUNCTION_OR_METHOD: &[SymbolKind] = &[SymbolKind::Function, SymbolKind::Method];
/// The kinds a signature change is reported for as a declaration change.
const DECLARATIONS: &[SymbolKind] = &[
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Union,
    SymbolKind::Trait,
    SymbolKind::Impl,
];
/// The containers a field can belong to.
const STRUCT_OR_VARIANT: &[SymbolKind] = &[SymbolKind::Struct, SymbolKind::Variant];
/// A struct, as the only container whose literal happens to miss a field.
const STRUCT_ONLY: &[SymbolKind] = &[SymbolKind::Struct];
/// An enum, as the only container whose variant a `match` misses.
const ENUM_ONLY: &[SymbolKind] = &[SymbolKind::Enum];
/// A trait, as the only container whose item an implementation can miss.
const TRAIT_ONLY: &[SymbolKind] = &[SymbolKind::Trait];

/// The rule matrix.
///
/// Four kinds of change reach nothing and therefore have no entry: a
/// formatting-only change, a widened visibility, a trait item that gained a
/// body, and an attribute change on anything but a function follow no edges.
pub(crate) static RULES: &[Rule] = &[
    // A body change reaches the callers of the changed function.
    Rule {
        applies: Applies {
            kind: ChangeKind::Body,
            detail: DetailMatch::Any,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::Calls]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::CallsChanged,
            continues: Continue::Always,
        }],
    },
    // A body change reaches the code that holds the function as a value.
    Rule {
        applies: Applies {
            kind: ChangeKind::Body,
            detail: DetailMatch::Any,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::TakesAsValue]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::TakesChangedAsValue,
            continues: Continue::Always,
        }],
    },
    // A body change reaches the code that may dispatch to it.
    Rule {
        applies: Applies {
            kind: ChangeKind::Body,
            detail: DetailMatch::Any,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::MayDispatchTo]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::DispatchesToChanged,
            continues: Continue::Always,
        }],
    },
    // A body change in a function that defines a procedural macro reaches what
    // that macro expands to. A seed is never expanded by `onward`, so the
    // changed function itself needs this first step; the C13 row covers only a
    // changed `macro_rules!` definition.
    //
    // The certainty is a candidate rather than the edge's own one. The edge is
    // resolved — rust-analyzer resolved the macro path — but a body change says
    // nothing certain about the tokens the macro now writes, which is the same
    // uncertainty C13 carries: the expansion may be identical. Taking the
    // resolved edge would report every invoker as likely affected when only the
    // expansion, not the reference, is in question.
    Rule {
        applies: Applies {
            kind: ChangeKind::Body,
            detail: DetailMatch::Any,
            symbol_kinds: FUNCTION_OR_METHOD,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::Expands]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::InvokesChangedMacro,
            continues: Continue::Always,
        }],
    },
    // A signature change makes the callers and holders adapt. A method that
    // implements a trait item is left out: its callers name the trait, whose
    // declaration did not change, so they compile unchanged.
    Rule {
        applies: Applies {
            kind: ChangeKind::Signature,
            detail: DetailMatch::Any,
            symbol_kinds: FUNCTION_OR_METHOD,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[EdgeKind::Calls]),
                filter: EdgeFilter::NotInTraitImpl,
                certainty: CertaintyRule::Edge,
                reason: Reason::SignatureChanged,
                continues: Continue::Always,
            },
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[EdgeKind::TakesAsValue]),
                filter: EdgeFilter::NotInTraitImpl,
                certainty: CertaintyRule::Edge,
                reason: Reason::SignatureChanged,
                continues: Continue::Always,
            },
        ],
    },
    // A signature change reaches the code that may dispatch to a method.
    Rule {
        applies: Applies {
            kind: ChangeKind::Signature,
            detail: DetailMatch::Any,
            symbol_kinds: FUNCTION_OR_METHOD,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::MayDispatchTo]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::DispatchesToChanged,
            continues: Continue::Always,
        }],
    },
    // A declaration change reaches everything that names the type.
    Rule {
        applies: Applies {
            kind: ChangeKind::Signature,
            detail: DetailMatch::Any,
            symbol_kinds: DECLARATIONS,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Always,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[
                EdgeKind::UsesType,
                EdgeKind::Constructs,
                EdgeKind::Matches,
                EdgeKind::Implements,
            ]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::DeclarationChanged,
            continues: Continue::Never,
        }],
    },
    // A narrowed visibility reaches the referrers it no longer covers.
    Rule {
        applies: Applies {
            kind: ChangeKind::Visibility,
            detail: DetailMatch::Narrowed,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Restricted,
        container_kinds: STRUCT_OR_VARIANT,
        sources: &[
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Any,
                filter: EdgeFilter::OutsideVisibility,
                certainty: CertaintyRule::Edge,
                reason: Reason::NoLongerVisible,
                continues: Continue::Never,
            },
            // A field is also named by the literals of its container, which
            // carry no edge of their own to the field: the literal has to
            // spell the name out, and the compiler rejects it once the field
            // is out of reach. A literal that ends with `..` may or may not
            // name it.
            Source {
                target: Target::Container,
                edges: EdgeSet::Kinds(&[EdgeKind::Constructs]),
                filter: EdgeFilter::OutsideVisibility,
                certainty: CertaintyRule::WithRestIsCandidate,
                reason: Reason::NoLongerVisible,
                continues: Continue::Never,
            },
        ],
    },
    // A removal reaches everything that refers to the symbol or to
    // anything inside it, on the base side.
    Rule {
        applies: Applies {
            kind: ChangeKind::Removed,
            detail: DetailMatch::Any,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Base,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::SelfAndDescendants,
            edges: EdgeSet::Any,
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::RefersToRemoved,
            continues: Continue::Never,
        }],
    },
    // A rename reaches what referred to the previous identity, on the
    // base side.
    Rule {
        applies: Applies {
            kind: ChangeKind::Renamed,
            detail: DetailMatch::Renamed,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Base,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Previous,
            edges: EdgeSet::Any,
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::RefersToRenamed,
            continues: Continue::Never,
        }],
    },
    // A removed field reaches its readers, writers and patterns, and
    // the literals of its container.
    Rule {
        applies: Applies {
            kind: ChangeKind::Field,
            detail: DetailMatch::Member(Delta::Removed),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Base,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: STRUCT_OR_VARIANT,
        sources: &[
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[
                    EdgeKind::ReadsField,
                    EdgeKind::WritesField,
                    EdgeKind::Matches,
                ]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::Edge,
                reason: Reason::UsesChangedField,
                continues: Continue::Never,
            },
            Source {
                target: Target::Container,
                edges: EdgeSet::Kinds(&[EdgeKind::Constructs]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::WithRestIsCandidate,
                reason: Reason::UsesChangedField,
                continues: Continue::Never,
            },
        ],
    },
    // the same on the head side, and only a field read
    // carries behaviour further.
    Rule {
        applies: Applies {
            kind: ChangeKind::Field,
            detail: DetailMatch::Member(Delta::Modified),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: STRUCT_OR_VARIANT,
        sources: &[
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[
                    EdgeKind::ReadsField,
                    EdgeKind::WritesField,
                    EdgeKind::Matches,
                ]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::Edge,
                reason: Reason::UsesChangedField,
                continues: Continue::Kinds(&[EdgeKind::ReadsField]),
            },
            Source {
                target: Target::Container,
                edges: EdgeSet::Kinds(&[EdgeKind::Constructs]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::WithRestIsCandidate,
                reason: Reason::UsesChangedField,
                continues: Continue::Never,
            },
        ],
    },
    // A new field reaches the literals that name every field.
    Rule {
        applies: Applies {
            kind: ChangeKind::Field,
            detail: DetailMatch::Member(Delta::Added),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: STRUCT_OR_VARIANT,
        sources: &[Source {
            target: Target::Container,
            edges: EdgeSet::Kinds(&[EdgeKind::Constructs]),
            filter: EdgeFilter::WithoutRest,
            certainty: CertaintyRule::Edge,
            reason: Reason::ConstructsWithoutField,
            continues: Continue::Never,
        }],
    },
    // A new field reaches the struct patterns that name every field.
    Rule {
        applies: Applies {
            kind: ChangeKind::Field,
            detail: DetailMatch::Member(Delta::Added),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: STRUCT_ONLY,
        sources: &[Source {
            target: Target::Container,
            edges: EdgeSet::Kinds(&[EdgeKind::Matches]),
            filter: EdgeFilter::WithoutRest,
            certainty: CertaintyRule::Edge,
            reason: Reason::PatternMissesField,
            continues: Continue::Never,
        }],
    },
    // A new variant reaches the exhaustive matches of the enum.
    Rule {
        applies: Applies {
            kind: ChangeKind::Variant,
            detail: DetailMatch::Member(Delta::Added),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ENUM_ONLY,
        sources: &[Source {
            target: Target::Container,
            edges: EdgeSet::Kinds(&[EdgeKind::Matches]),
            filter: EdgeFilter::Detail(EdgeDetail::MatchExhaustive),
            certainty: CertaintyRule::Edge,
            reason: Reason::ExhaustiveMatchMissesVariant,
            continues: Continue::Never,
        }],
    },
    // A new variant reaches the matches whose catch-all now catches it.
    Rule {
        applies: Applies {
            kind: ChangeKind::Variant,
            detail: DetailMatch::Member(Delta::Added),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ENUM_ONLY,
        sources: &[Source {
            target: Target::Container,
            edges: EdgeSet::Kinds(&[EdgeKind::Matches]),
            filter: EdgeFilter::Detail(EdgeDetail::MatchWildcard),
            certainty: CertaintyRule::Edge,
            reason: Reason::WildcardCatchesNewVariant,
            continues: Continue::Always,
        }],
    },
    // A removed variant reaches everything that used it, on the base
    // side.
    Rule {
        applies: Applies {
            kind: ChangeKind::Variant,
            detail: DetailMatch::Member(Delta::Removed),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Base,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::SelfAndDescendants,
            edges: EdgeSet::Any,
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::UsesChangedVariant,
            continues: Continue::Never,
        }],
    },
    // A reshaped variant reaches everything that uses it.
    Rule {
        applies: Applies {
            kind: ChangeKind::Variant,
            detail: DetailMatch::Member(Delta::Modified),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::SelfAndDescendants,
            edges: EdgeSet::Any,
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::UsesChangedVariant,
            continues: Continue::Never,
        }],
    },
    // A required trait item that appeared reaches the implementations
    // of the trait.
    Rule {
        applies: Applies {
            kind: ChangeKind::TraitItem,
            detail: DetailMatch::Member(Delta::Added),
            symbol_kinds: ANY_KIND,
            has_body: Some(false),
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: TRAIT_ONLY,
        sources: &[Source {
            target: Target::Container,
            edges: EdgeSet::Kinds(&[EdgeKind::Implements]),
            filter: EdgeFilter::FromImpl,
            certainty: CertaintyRule::Edge,
            reason: Reason::MustImplementTraitItem,
            continues: Continue::Never,
        }],
    },
    // A changed trait item reaches the implementations that provide it
    // and the code that calls it.
    Rule {
        applies: Applies {
            kind: ChangeKind::TraitItem,
            detail: DetailMatch::Member(Delta::Modified),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: TRAIT_ONLY,
        sources: &[
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[EdgeKind::Implements]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::Edge,
                reason: Reason::MustImplementTraitItem,
                continues: Continue::Never,
            },
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[EdgeKind::Calls, EdgeKind::TakesAsValue]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::Edge,
                reason: Reason::UsesChangedTraitItem,
                continues: Continue::Always,
            },
        ],
    },
    // A removed trait item reaches everything that used it, on the base
    // side.
    Rule {
        applies: Applies {
            kind: ChangeKind::TraitItem,
            detail: DetailMatch::Member(Delta::Removed),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Base,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Any,
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::UsesChangedTraitItem,
            continues: Continue::Never,
        }],
    },
    // A changed default body reaches the implementations that do not
    // override it.
    Rule {
        applies: Applies {
            kind: ChangeKind::TraitItem,
            detail: DetailMatch::DefaultBody,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: TRAIT_ONLY,
        sources: &[Source {
            target: Target::Container,
            edges: EdgeSet::Kinds(&[EdgeKind::Implements]),
            filter: EdgeFilter::ImplDoesNotOverride,
            certainty: CertaintyRule::Edge,
            reason: Reason::InheritsChangedDefault,
            continues: Continue::Never,
        }],
    },
    // A changed default body reaches its callers, less surely when an
    // implementation replaces the default.
    Rule {
        applies: Applies {
            kind: ChangeKind::TraitItem,
            detail: DetailMatch::DefaultBody,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: TRAIT_ONLY,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::Calls]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::OverrideIsCandidate,
            reason: Reason::CallsChanged,
            continues: Continue::Always,
        }],
    },
    // A changed default body reaches the code that holds it as a value.
    Rule {
        applies: Applies {
            kind: ChangeKind::TraitItem,
            detail: DetailMatch::DefaultBody,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: TRAIT_ONLY,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::TakesAsValue]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::TakesChangedAsValue,
            continues: Continue::Always,
        }],
    },
    // An implementation that appeared changes the resolution of every
    // reference to its items.
    Rule {
        applies: Applies {
            kind: ChangeKind::Impl,
            detail: DetailMatch::Member(Delta::Added),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Children,
            edges: EdgeSet::Kinds(&[
                EdgeKind::Calls,
                EdgeKind::TakesAsValue,
                EdgeKind::MayDispatchTo,
            ]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::ImplResolutionChanged,
            continues: Continue::Always,
        }],
    },
    // the same for an implementation that disappeared, read
    // on the base side.
    Rule {
        applies: Applies {
            kind: ChangeKind::Impl,
            detail: DetailMatch::Member(Delta::Removed),
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Base,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Children,
            edges: EdgeSet::Kinds(&[
                EdgeKind::Calls,
                EdgeKind::TakesAsValue,
                EdgeKind::MayDispatchTo,
            ]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::ImplResolutionChanged,
            continues: Continue::Always,
        }],
    },
    // A changed constant value reaches the code that reads it.
    Rule {
        applies: Applies {
            kind: ChangeKind::ConstValue,
            detail: DetailMatch::None,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::UsesValue]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::UsesChangedValue,
            continues: Continue::Always,
        }],
    },
    // A retyped constant makes the code that reads it adapt.
    Rule {
        applies: Applies {
            kind: ChangeKind::ConstValue,
            detail: DetailMatch::Retyped,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::UsesValue]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Edge,
            reason: Reason::UsesRetypedValue,
            continues: Continue::Always,
        }],
    },
    // A changed type alias reaches the code that names it.
    Rule {
        applies: Applies {
            kind: ChangeKind::TypeAlias,
            detail: DetailMatch::Any,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Adaptation,
        coarse: Coarse::Always,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::UsesType]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::UsesChangedAlias,
            continues: Continue::Never,
        }],
    },
    // A changed macro definition reaches the code that invokes it.
    Rule {
        applies: Applies {
            kind: ChangeKind::MacroDef,
            detail: DetailMatch::Any,
            symbol_kinds: ANY_KIND,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[Source {
            target: Target::Symbol,
            edges: EdgeSet::Kinds(&[EdgeKind::Expands]),
            filter: EdgeFilter::None,
            certainty: CertaintyRule::Candidate,
            reason: Reason::InvokesChangedMacro,
            continues: Continue::Always,
        }],
    },
    // A changed attribute on a function reaches what a body change reaches,
    // as a candidate.
    Rule {
        applies: Applies {
            kind: ChangeKind::Attribute,
            detail: DetailMatch::Any,
            symbol_kinds: FUNCTION_OR_METHOD,
            has_body: None,
        },
        side: Side::Head,
        class: ImpactClass::Behaviour,
        coarse: Coarse::Never,
        container_kinds: ANY_KIND,
        sources: &[
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[EdgeKind::Calls]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::Candidate,
                reason: Reason::AttributeChanged,
                continues: Continue::Always,
            },
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[EdgeKind::TakesAsValue]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::Candidate,
                reason: Reason::AttributeChanged,
                continues: Continue::Always,
            },
            Source {
                target: Target::Symbol,
                edges: EdgeSet::Kinds(&[EdgeKind::MayDispatchTo]),
                filter: EdgeFilter::None,
                certainty: CertaintyRule::Candidate,
                reason: Reason::AttributeChanged,
                continues: Continue::Always,
            },
        ],
    },
];

/// One row of the matrix: the changes it applies to, the graph it reads, and
/// the groups of edges it follows.
pub(crate) struct Rule {
    /// The changes the row applies to.
    applies: Applies,
    /// The graph the row reads.
    side: Side,
    /// The class the reached items get when the path stops here.
    class: ImpactClass,
    /// When the row counts as coarse.
    coarse: Coarse,
    /// The kinds the container of the changed symbol must have; empty for any.
    container_kinds: &'static [SymbolKind],
    /// The groups of edges the row follows.
    sources: &'static [Source],
}

/// The changes a row applies to.
struct Applies {
    /// The kind of change.
    kind: ChangeKind,
    /// The detail the change must carry.
    detail: DetailMatch,
    /// The kinds the changed symbol must have; empty for any.
    symbol_kinds: &'static [SymbolKind],
    /// Whether the changed symbol must have a body; `None` for any.
    has_body: Option<bool>,
}

impl Applies {
    /// Whether the row applies to `change`, ignoring the body condition.
    fn matches(&self, change: &Change) -> bool {
        change.kind == self.kind
            && self.detail.matches(&change.detail)
            && (self.symbol_kinds.is_empty() || self.symbol_kinds.contains(&change.symbol.kind))
    }
}

/// The detail a change must carry for a row to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailMatch {
    /// Any detail.
    Any,
    /// No detail.
    None,
    /// A member that appeared, disappeared or changed.
    Member(Delta),
    /// Only the body of a provided trait method changed.
    DefaultBody,
    /// A declared type changed.
    Retyped,
    /// A visibility was narrowed.
    Narrowed,
    /// A symbol moved to another identity.
    Renamed,
}

impl DetailMatch {
    /// Whether `detail` is the one this matcher wants.
    fn matches(self, detail: &ChangeDetail) -> bool {
        match (self, detail) {
            (DetailMatch::Any, _) => true,
            (DetailMatch::None, ChangeDetail::None) => true,
            (DetailMatch::Member(wanted), ChangeDetail::Member(actual)) => wanted == *actual,
            (DetailMatch::DefaultBody, ChangeDetail::DefaultBody) => true,
            (DetailMatch::Retyped, ChangeDetail::Retyped) => true,
            (DetailMatch::Narrowed, ChangeDetail::Visibility { from, to }) => *to < *from,
            (DetailMatch::Renamed, ChangeDetail::Renamed { .. }) => true,
            _ => false,
        }
    }
}

/// Where the edges a row follows must arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The changed symbol itself.
    Symbol,
    /// The container of the changed symbol.
    Container,
    /// The changed symbol and everything inside it.
    SelfAndDescendants,
    /// The direct children of the changed symbol.
    Children,
    /// The previous identity of a renamed symbol.
    Previous,
}

/// One group of edges a row follows.
struct Source {
    /// Where the edges must arrive.
    target: Target,
    /// The kinds to follow.
    edges: EdgeSet,
    /// The extra conditions an edge must meet.
    filter: EdgeFilter,
    /// How certain the step is.
    certainty: CertaintyRule,
    /// Why the step applies.
    reason: Reason,
    /// Whether behaviour propagation continues from the reached item.
    continues: Continue,
}

/// The kinds of edge a group follows.
#[derive(Debug, Clone, Copy)]
enum EdgeSet {
    /// Every kind except `Contains`.
    Any,
    /// Exactly these kinds.
    Kinds(&'static [EdgeKind]),
}

impl EdgeSet {
    /// Whether `kind` belongs to this set.
    fn contains(self, kind: EdgeKind) -> bool {
        match self {
            EdgeSet::Any => kind != EdgeKind::Contains,
            EdgeSet::Kinds(kinds) => kinds.contains(&kind),
        }
    }
}

/// The extra condition an edge must meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeFilter {
    /// Every edge of the right kind.
    None,
    /// Only edges carrying this detail.
    Detail(EdgeDetail),
    /// Only edges that do not carry `WithRest`.
    WithoutRest,
    /// Only edges whose referrer is outside the new visibility of the change.
    OutsideVisibility,
    /// No edge at all when the changed symbol implements a trait item.
    ///
    /// The caller of such a method names the trait, not the implementation:
    /// its call resolves through the trait declaration, which the change does
    /// not touch, so it compiles unchanged. Only a change to the trait item
    /// itself makes callers adapt.
    NotInTraitImpl,
    /// Only edges emitted by an implementation block.
    FromImpl,
    /// Only edges emitted by an implementation that does not override the
    /// changed trait item.
    ImplDoesNotOverride,
}

/// How certain a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CertaintyRule {
    /// The certainty of the edge.
    Edge,
    /// Always a candidate.
    Candidate,
    /// The certainty of the edge, but a candidate when it carries `WithRest`.
    WithRestIsCandidate,
    /// The certainty of the edge, but a candidate when an implementation of the
    /// container replaces the changed item.
    OverrideIsCandidate,
}

/// Whether behaviour propagation continues from a reached item.
#[derive(Debug, Clone, Copy)]
enum Continue {
    /// Never.
    Never,
    /// From every reached item.
    Always,
    /// Only from items reached by one of these edge kinds.
    Kinds(&'static [EdgeKind]),
}

impl Continue {
    /// Whether a step over an edge of `kind` continues.
    fn allows(self, kind: EdgeKind) -> bool {
        match self {
            Continue::Never => false,
            Continue::Always => true,
            Continue::Kinds(kinds) => kinds.contains(&kind),
        }
    }
}

/// When a row counts as coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coarse {
    /// Never.
    Never,
    /// Always.
    Always,
    /// Only when the new visibility is `Restricted`.
    Restricted,
}

/// The first steps a change takes: the dependencies that arrive at what the
/// change is about.
///
/// The result is ordered by side and edge index, so it does not depend on the
/// order of the table.
pub(crate) fn first_steps<'a>(change: &'a Change, ctx: &'a Graphs<'a>) -> Vec<Reach> {
    let mut reaches = Vec::new();
    for rule in RULES {
        if !rule.applies.matches(change) {
            continue;
        }
        let scope = Scope::new(rule, change, ctx);
        if !scope.has_expected_body() {
            continue;
        }
        for source in rule.sources {
            scope.follow(source, &mut reaches);
        }
    }
    reaches.sort_by_key(|reach| (reach.side, reach.edge));
    reaches.dedup_by(|left, right| left.side == right.side && left.edge == right.edge);
    reaches
}

/// The behaviour-propagation step: the references that reach `item` on the head
/// graph.
///
/// A call or a value read is certain, passing the item around as a value and
/// dispatching to it through a trait are not, and every item reached this way
/// continues the propagation.
///
/// An `Expands` edge is followed as well, so that a change anywhere in a
/// procedural macro's implementation continues from the macro function to the
/// items whose expansion names it. The edge is resolved, but whether the change
/// alters the expansion is not, so the step is a candidate like C13's first
/// step.
pub(crate) fn onward(item: SymbolId, ctx: &Graphs<'_>) -> Vec<Reach> {
    let graph = ctx.head;
    let mut reaches = Vec::new();
    for &edge in ctx.head_index.edges_to(item) {
        let Some(reference) = graph.edges.get(edge as usize) else {
            continue;
        };
        let (certainty, reason) = match reference.kind {
            EdgeKind::Calls => (reference.certainty, Reason::CallsChanged),
            EdgeKind::TakesAsValue => (Certainty::Candidate, Reason::TakesChangedAsValue),
            EdgeKind::MayDispatchTo => (Certainty::Candidate, Reason::DispatchesToChanged),
            EdgeKind::UsesValue => (reference.certainty, Reason::UsesChangedValue),
            EdgeKind::Expands => (Certainty::Candidate, Reason::InvokesChangedMacro),
            _ => continue,
        };
        if is_self_or_descendant(graph, reference.from, item) {
            continue;
        }
        reaches.push(Reach {
            side: Side::Head,
            edge,
            class: ImpactClass::Behaviour,
            certainty,
            reason,
            continues: true,
            coarse: false,
        });
    }
    reaches
}

/// One row applied to one change, with the symbols the row needs already looked
/// up on the side it reads.
struct Scope<'a> {
    /// The row being applied.
    rule: &'static Rule,
    /// The change being applied.
    change: &'a Change,
    /// The graphs of the comparison.
    ctx: &'a Graphs<'a>,
    /// The changed symbol on the side the row reads.
    symbol: Option<SymbolId>,
    /// The container of the changed symbol on that side.
    container: Option<SymbolId>,
    /// The previous identity of a renamed symbol, on the base side.
    previous: Option<SymbolId>,
}

impl<'a> Scope<'a> {
    /// Looks up what the row needs.
    fn new(rule: &'static Rule, change: &'a Change, ctx: &'a Graphs<'a>) -> Scope<'a> {
        let graph = ctx.graph(rule.side);
        let index = ctx.index(rule.side);
        let symbol = index.by_path(&change.symbol);
        let container = container_of(graph, index, change, symbol, rule.container_kinds);
        let previous = match &change.detail {
            ChangeDetail::Renamed { from } => ctx.base_index.by_path(from),
            _ => None,
        };
        Scope {
            rule,
            change,
            ctx,
            symbol,
            container,
            previous,
        }
    }

    /// Whether the changed symbol has the body the row expects, when the row
    /// cares.
    fn has_expected_body(&self) -> bool {
        let Some(expected) = self.rule.applies.has_body else {
            return true;
        };
        let graph = self.ctx.graph(self.rule.side);
        self.symbol
            .and_then(|symbol| graph.symbols.get(symbol.0 as usize))
            .is_some_and(|symbol| symbol.flags.has_body == expected)
    }

    /// Adds a reach for every edge of `source` that arrives at one of the
    /// source's targets.
    fn follow(&self, source: &Source, out: &mut Vec<Reach>) {
        let side = self.rule.side;
        let graph = self.ctx.graph(side);
        let index = self.ctx.index(side);
        for target in self.targets(source.target) {
            for &edge in index.edges_to(target) {
                let Some(reference) = graph.edges.get(edge as usize) else {
                    continue;
                };
                if !source.edges.contains(reference.kind) {
                    continue;
                }
                if is_self_or_descendant(graph, reference.from, target) {
                    continue;
                }
                let Some(certainty) = self.judge(source, reference) else {
                    continue;
                };
                out.push(Reach {
                    side,
                    edge,
                    class: self.rule.class,
                    certainty,
                    reason: source.reason,
                    continues: source.continues.allows(reference.kind),
                    coarse: self.is_coarse(),
                });
            }
        }
    }

    /// The symbols whose incoming edges the group follows.
    fn targets(&self, target: Target) -> Vec<SymbolId> {
        let index = self.ctx.index(self.rule.side);
        match target {
            Target::Symbol => self.symbol.into_iter().collect(),
            Target::Container => self.container.into_iter().collect(),
            Target::Previous => self.previous.into_iter().collect(),
            Target::SelfAndDescendants => match self.symbol {
                Some(symbol) => descendants(index, symbol),
                None => Vec::new(),
            },
            Target::Children => match self.symbol {
                Some(symbol) => index.children(symbol).to_vec(),
                None => Vec::new(),
            },
        }
    }

    /// The certainty of the step over `edge`, or `None` when the group's filter
    /// rejects the edge.
    fn judge(&self, source: &Source, edge: &Edge) -> Option<Certainty> {
        if !self.passes(source.filter, edge) {
            return None;
        }
        let certainty = match source.certainty {
            CertaintyRule::Edge => edge.certainty,
            CertaintyRule::Candidate => Certainty::Candidate,
            CertaintyRule::WithRestIsCandidate => {
                if edge.detail == EdgeDetail::WithRest {
                    Certainty::Candidate
                } else {
                    edge.certainty
                }
            }
            CertaintyRule::OverrideIsCandidate => {
                if self.is_overridden() {
                    Certainty::Candidate
                } else {
                    edge.certainty
                }
            }
        };
        Some(certainty)
    }

    /// Whether the edge passes the group's filter.
    fn passes(&self, filter: EdgeFilter, edge: &Edge) -> bool {
        match filter {
            EdgeFilter::None => true,
            EdgeFilter::Detail(detail) => edge.detail == detail,
            EdgeFilter::WithoutRest => edge.detail != EdgeDetail::WithRest,
            EdgeFilter::OutsideVisibility => self.outside_new_visibility(edge.from),
            EdgeFilter::NotInTraitImpl => !self.in_trait_impl(),
            EdgeFilter::FromImpl => self.kind_of(edge.from) == Some(SymbolKind::Impl),
            EdgeFilter::ImplDoesNotOverride => self.impl_does_not_override(edge.from),
        }
    }

    /// Whether the row counts as coarse for this change.
    fn is_coarse(&self) -> bool {
        match self.rule.coarse {
            Coarse::Never => false,
            Coarse::Always => true,
            Coarse::Restricted => matches!(
                &self.change.detail,
                ChangeDetail::Visibility {
                    to: Visibility::Restricted,
                    ..
                }
            ),
        }
    }

    /// The kind of a symbol on the side the row reads.
    fn kind_of(&self, id: SymbolId) -> Option<SymbolKind> {
        self.ctx
            .graph(self.rule.side)
            .symbols
            .get(id.0 as usize)
            .map(|symbol| symbol.path.kind)
    }

    /// Whether `referrer` can no longer see the changed symbol under its new
    /// visibility.
    fn outside_new_visibility(&self, referrer: SymbolId) -> bool {
        let head = self.ctx.head;
        let Some(symbol) = self.symbol else {
            return false;
        };
        let ChangeDetail::Visibility { to, .. } = &self.change.detail else {
            return false;
        };
        let referrer_module = enclosing_module(head, referrer);
        match *to {
            Visibility::Private => {
                let Some(boundary) = enclosing_module(head, symbol) else {
                    return true;
                };
                referrer_module.is_none_or(|module| !is_self_or_descendant(head, module, boundary))
            }
            Visibility::Restricted => {
                let boundary = enclosing_module(head, symbol)
                    .and_then(|module| enclosing_module(head, module));
                let Some(boundary) = boundary else {
                    return true;
                };
                referrer_module.is_none_or(|module| !is_self_or_descendant(head, module, boundary))
            }
            Visibility::Crate => {
                let (Some(referrer), Some(changed)) = (
                    head.symbols.get(referrer.0 as usize),
                    head.symbols.get(symbol.0 as usize),
                ) else {
                    return false;
                };
                referrer.krate != changed.krate
            }
            Visibility::Public => false,
        }
    }

    /// Whether the changed symbol implements a trait item.
    fn in_trait_impl(&self) -> bool {
        self.symbol
            .and_then(|id| self.ctx.graph(self.rule.side).symbols.get(id.0 as usize))
            .is_some_and(|symbol| symbol.flags.in_trait_impl)
    }

    /// Whether any implementation of the container replaces the changed trait
    /// item.
    fn is_overridden(&self) -> bool {
        let (Some(container), Some(symbol)) = (self.container, self.symbol) else {
            return false;
        };
        self.implementation_blocks(container)
            .into_iter()
            .any(|block| self.children_implement(block, symbol))
    }

    /// Whether `block` keeps the default of the changed trait item.
    fn impl_does_not_override(&self, block: SymbolId) -> bool {
        if self.kind_of(block) != Some(SymbolKind::Impl) {
            return false;
        }
        match self.symbol {
            Some(symbol) => !self.children_implement(block, symbol),
            None => true,
        }
    }

    /// Whether `block` has a child with an `Implements` edge to `symbol`.
    fn children_implement(&self, block: SymbolId, symbol: SymbolId) -> bool {
        let graph = self.ctx.graph(self.rule.side);
        let index = self.ctx.index(self.rule.side);
        index.children(block).iter().any(|&child| {
            index.edges_from(child).iter().any(|&edge| {
                graph.edges.get(edge as usize).is_some_and(|reference| {
                    reference.kind == EdgeKind::Implements && reference.to == symbol
                })
            })
        })
    }

    /// The implementation blocks of `container`, which must be a trait.
    fn implementation_blocks(&self, container: SymbolId) -> Vec<SymbolId> {
        let graph = self.ctx.graph(self.rule.side);
        let index = self.ctx.index(self.rule.side);
        index
            .edges_to(container)
            .iter()
            .filter_map(|&edge| graph.edges.get(edge as usize))
            .filter(|reference| reference.kind == EdgeKind::Implements)
            .map(|reference| reference.from)
            .filter(|&from| {
                graph
                    .symbols
                    .get(from.0 as usize)
                    .is_some_and(|symbol| symbol.path.kind == SymbolKind::Impl)
            })
            .collect()
    }
}

/// The container of the changed symbol: its parent, or the symbol named by its
/// path without the last segment when the change is about a symbol the graph
/// does not know.
fn container_of(
    graph: &RevisionGraph,
    index: &GraphIndex,
    change: &Change,
    symbol: Option<SymbolId>,
    kinds: &[SymbolKind],
) -> Option<SymbolId> {
    if let Some(id) = symbol {
        let parent = graph.symbols.get(id.0 as usize)?.parent?;
        let parent_kind = graph.symbols.get(parent.0 as usize)?.path.kind;
        return (kinds.is_empty() || kinds.contains(&parent_kind)).then_some(parent);
    }
    let text = container_text(&change.symbol)?;
    let candidates = index.find(&text);
    if kinds.is_empty() {
        return candidates.first().copied();
    }
    candidates.iter().copied().find(|&id| {
        graph
            .symbols
            .get(id.0 as usize)
            .is_some_and(|symbol| kinds.contains(&symbol.path.kind))
    })
}

/// The path text of a symbol's container: the path without its last segment.
fn container_text(path: &SymbolPath) -> Option<String> {
    let text = path.path.as_str();
    let name = path.name();
    let rest = text.strip_suffix(name)?;
    let parent = rest.strip_suffix("::")?;
    if parent.is_empty() {
        return None;
    }
    Some(parent.to_owned())
}

/// `id` and every symbol inside it, the symbol first.
fn descendants(index: &GraphIndex, id: SymbolId) -> Vec<SymbolId> {
    let mut out = vec![id];
    let mut cursor = 0;
    while let Some(&current) = out.get(cursor) {
        for &child in index.children(current) {
            if !out.contains(&child) {
                out.push(child);
            }
        }
        cursor += 1;
    }
    out
}

/// Whether `id` is `ancestor` itself or lies inside it by `parent`.
fn is_self_or_descendant(graph: &RevisionGraph, id: SymbolId, ancestor: SymbolId) -> bool {
    if id == ancestor {
        return true;
    }
    let mut current = graph
        .symbols
        .get(id.0 as usize)
        .and_then(|symbol| symbol.parent);
    while let Some(parent) = current {
        if parent == ancestor {
            return true;
        }
        current = graph
            .symbols
            .get(parent.0 as usize)
            .and_then(|symbol| symbol.parent);
    }
    false
}

/// The nearest ancestor of `id` that is a module.
fn enclosing_module(graph: &RevisionGraph, id: SymbolId) -> Option<SymbolId> {
    let mut current = graph
        .symbols
        .get(id.0 as usize)
        .and_then(|symbol| symbol.parent);
    while let Some(parent) = current {
        let symbol = graph.symbols.get(parent.0 as usize)?;
        if symbol.path.kind == SymbolKind::Module {
            return Some(parent);
        }
        current = symbol.parent;
    }
    None
}

/// Test helpers that build small graphs and changes by hand.
#[cfg(test)]
pub(crate) mod testkit {
    use blastr_model::{
        Certainty, Change, ChangeDetail, ChangeKind, CrateId, CrateInfo, Delta, Edge, EdgeDetail,
        EdgeKind, Location, RelPath, RevisionGraph, Span, Symbol, SymbolFlags, SymbolId,
        SymbolKind, SymbolPath, TargetKind, Visibility,
    };

    use super::{Graphs, Reach, Side, first_steps};

    /// Builds a [`RevisionGraph`] one symbol and one edge at a time.
    pub(crate) struct GraphBuilder {
        graph: RevisionGraph,
    }

    impl GraphBuilder {
        /// A builder whose graph holds one crate named `krate`.
        pub(crate) fn new() -> GraphBuilder {
            let mut graph = RevisionGraph::default();
            graph.crates.push(CrateInfo {
                name: "krate".to_owned(),
                package: "krate".to_owned(),
                target_kind: TargetKind::Lib,
                target_name: "krate".to_owned(),
                root_file: RelPath::new("src/lib.rs"),
            });
            GraphBuilder { graph }
        }

        /// Adds a symbol in `src/lib.rs` without a parent.
        pub(crate) fn add(&mut self, path: &str, kind: SymbolKind) -> SymbolId {
            let id = SymbolId(self.graph.symbols.len() as u32);
            self.graph.symbols.push(Symbol {
                path: SymbolPath::new(path, kind),
                krate: CrateId(0),
                location: Location {
                    file: RelPath::new("src/lib.rs"),
                    span: Span {
                        start: 0,
                        end: 10,
                        start_line: 1,
                        end_line: 1,
                    },
                },
                visibility: Visibility::Private,
                signature_hash: 0,
                body_hash: 0,
                attr_hash: 0,
                body_sketch: Vec::new(),
                parent: None,
                flags: SymbolFlags::default(),
                impl_info: None,
            });
            id
        }

        /// Adds a symbol inside `parent`.
        pub(crate) fn child(&mut self, parent: SymbolId, path: &str, kind: SymbolKind) -> SymbolId {
            let id = self.add(path, kind);
            self.symbol(id).parent = Some(parent);
            id
        }

        /// One symbol while it is being described.
        pub(crate) fn symbol(&mut self, id: SymbolId) -> &mut Symbol {
            &mut self.graph.symbols[id.0 as usize]
        }

        /// Puts the symbol in `file`.
        pub(crate) fn file(&mut self, id: SymbolId, file: &str) {
            self.symbol(id).location.file = RelPath::new(file);
        }

        /// Gives the symbol a body.
        pub(crate) fn body(&mut self, id: SymbolId) {
            self.symbol(id).flags.has_body = true;
        }

        /// Puts the symbol in another crate, adding the crate if needed.
        pub(crate) fn krate(&mut self, id: SymbolId, index: u32) {
            while self.graph.crates.len() <= index as usize {
                let number = self.graph.crates.len();
                self.graph.crates.push(CrateInfo {
                    name: format!("krate{number}"),
                    package: format!("krate{number}"),
                    target_kind: TargetKind::Lib,
                    target_name: format!("krate{number}"),
                    root_file: RelPath::new("src/lib.rs"),
                });
            }
            self.symbol(id).krate = CrateId(index);
        }

        /// Adds a resolved edge.
        pub(crate) fn edge(
            &mut self,
            from: SymbolId,
            to: SymbolId,
            kind: EdgeKind,
            detail: EdgeDetail,
        ) {
            self.push(from, to, kind, detail, Certainty::Resolved);
        }

        /// Adds a candidate edge.
        pub(crate) fn candidate(
            &mut self,
            from: SymbolId,
            to: SymbolId,
            kind: EdgeKind,
            detail: EdgeDetail,
        ) {
            self.push(from, to, kind, detail, Certainty::Candidate);
        }

        /// Adds an edge of the given certainty.
        fn push(
            &mut self,
            from: SymbolId,
            to: SymbolId,
            kind: EdgeKind,
            detail: EdgeDetail,
            certainty: Certainty,
        ) {
            self.graph.edges.push(Edge {
                from,
                to,
                kind,
                certainty,
                site: Location {
                    file: RelPath::new("src/lib.rs"),
                    span: Span {
                        start: 1,
                        end: 2,
                        start_line: 1,
                        end_line: 1,
                    },
                },
                detail,
            });
        }

        /// The graph built so far.
        pub(crate) fn finish(self) -> RevisionGraph {
            self.graph
        }
    }

    /// A change of `kind` to the symbol at `path`, without a detail.
    pub(crate) fn change(kind: ChangeKind, path: &str, symbol_kind: SymbolKind) -> Change {
        detailed(kind, path, symbol_kind, ChangeDetail::None)
    }

    /// A change that carries `detail`.
    pub(crate) fn detailed(
        kind: ChangeKind,
        path: &str,
        symbol_kind: SymbolKind,
        detail: ChangeDetail,
    ) -> Change {
        Change {
            kind,
            symbol: SymbolPath::new(path, symbol_kind),
            old: None,
            new: None,
            detail,
        }
    }

    /// A changed member of a field, variant, trait item or implementation.
    pub(crate) fn member(
        kind: ChangeKind,
        path: &str,
        symbol_kind: SymbolKind,
        delta: Delta,
    ) -> Change {
        detailed(kind, path, symbol_kind, ChangeDetail::Member(delta))
    }

    /// The first steps of `change` over the two graphs.
    pub(crate) fn first(base: &RevisionGraph, head: &RevisionGraph, change: &Change) -> Vec<Reach> {
        let graphs = Graphs::new(base, head);
        first_steps(change, &graphs)
    }

    /// The path of the symbol one step reaches.
    pub(crate) fn step_path(reach: &Reach, base: &RevisionGraph, head: &RevisionGraph) -> String {
        let graph = match reach.side {
            Side::Base => base,
            Side::Head => head,
        };
        let from = graph
            .edges
            .get(reach.edge as usize)
            .map(|reference| reference.from);
        from.and_then(|id| graph.symbols.get(id.0 as usize))
            .map(|symbol| symbol.path.path.clone())
            .unwrap_or_default()
    }

    /// The paths the steps reach, sorted.
    pub(crate) fn reached_paths(
        steps: &[Reach],
        base: &RevisionGraph,
        head: &RevisionGraph,
    ) -> Vec<String> {
        let mut paths: Vec<String> = steps
            .iter()
            .map(|reach| step_path(reach, base, head))
            .collect();
        paths.sort();
        paths
    }

    /// The step that reaches the symbol at `path`.
    pub(crate) fn step_to<'s>(
        steps: &'s [Reach],
        base: &RevisionGraph,
        head: &RevisionGraph,
        path: &str,
    ) -> &'s Reach {
        steps
            .iter()
            .find(|reach| step_path(reach, base, head) == path)
            .unwrap_or_else(|| panic!("no step reaches `{path}`"))
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        Certainty, ChangeDetail, ChangeKind, Delta, EdgeDetail, EdgeKind, ImpactClass, Reason,
        RevisionGraph, SymbolKind, SymbolPath, Visibility,
    };

    use super::testkit::{GraphBuilder, change, detailed, first, member, reached_paths, step_to};
    use super::*;

    /// An empty graph, for rules that read one side only.
    fn nothing() -> RevisionGraph {
        RevisionGraph::default()
    }

    #[test]
    fn row_2_a_body_change_reaches_its_callers() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let user = builder.child(module, "krate::User", SymbolKind::Struct);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(user, function, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller"]
        );
        let step = step_to(&steps, &nothing(), &head, "krate::caller");
        assert_eq!(step.class, ImpactClass::Behaviour);
        assert_eq!(step.certainty, Certainty::Resolved);
        assert_eq!(step.reason, Reason::CallsChanged);
        assert!(step.continues);
        assert!(!step.coarse);
    }

    #[test]
    fn row_3_a_body_change_reaches_holders_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(reader, function, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::holder"]
        );
        let step = step_to(&steps, &nothing(), &head, "krate::holder");
        assert_eq!(step.certainty, Certainty::Candidate);
        assert_eq!(step.reason, Reason::TakesChangedAsValue);
        assert_eq!(step.class, ImpactClass::Behaviour);
        assert!(step.continues);
    }

    #[test]
    fn row_4_a_body_change_reaches_dispatch_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let method = builder.child(module, "krate::f", SymbolKind::Method);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let user = builder.child(module, "krate::User", SymbolKind::Struct);
        builder.edge(
            dispatcher,
            method,
            EdgeKind::MayDispatchTo,
            EdgeDetail::None,
        );
        builder.edge(user, method, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Method);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::dispatcher"]
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate)
        );
        assert!(!steps.is_empty());
        assert_eq!(steps[0].reason, Reason::DispatchesToChanged);
        assert!(steps[0].continues);
    }

    #[test]
    fn a_body_change_in_a_macro_function_reaches_its_expansions() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let derive = builder.child(module, "krate::derive_name", SymbolKind::Function);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let block = builder.child(module, "krate::<impl Name for Point>", SymbolKind::Impl);
        builder.edge(point, derive, EdgeKind::Expands, EdgeDetail::None);
        builder.edge(block, derive, EdgeKind::Expands, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::derive_name", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::<impl Name for Point>", "krate::Point"]
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate),
            "a body change does not prove the expansion changed"
        );
        assert!(
            steps
                .iter()
                .all(|step| step.reason == Reason::InvokesChangedMacro)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.class == ImpactClass::Behaviour && step.continues)
        );
    }

    #[test]
    fn a_body_change_in_something_that_is_not_a_function_follows_no_expansion() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let macro_ = builder.child(module, "krate::declare", SymbolKind::Macro);
        let invoker = builder.child(module, "krate::invoker", SymbolKind::Function);
        builder.edge(invoker, macro_, EdgeKind::Expands, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::declare", SymbolKind::Macro);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn row_5_a_signature_change_reaches_callers_and_holders() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(reader, function, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Signature, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller", "krate::holder"],
            "an edge of a kind a signature change does not follow is not reached"
        );
        for path in ["krate::caller", "krate::holder"] {
            let step = step_to(&steps, &nothing(), &head, path);
            assert_eq!(step.class, ImpactClass::Adaptation);
            assert_eq!(step.certainty, Certainty::Resolved);
            assert_eq!(step.reason, Reason::SignatureChanged);
            assert!(step.continues);
        }
    }

    #[test]
    fn row_5_a_signature_change_to_a_trait_impl_method_leaves_callers_alone() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let stage = builder.child(module, "krate::Stage", SymbolKind::Trait);
        let item = builder.child(stage, "krate::Stage::apply", SymbolKind::Method);
        let block = builder.child(module, "krate::<impl Stage for V>", SymbolKind::Impl);
        let method = builder.child(
            block,
            "krate::<impl Stage for V>::apply",
            SymbolKind::Method,
        );
        builder.symbol(method).flags.in_trait_impl = true;
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        builder.edge(block, stage, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(method, item, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(caller, method, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, method, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let change = change(
            ChangeKind::Signature,
            "krate::<impl Stage for V>::apply",
            SymbolKind::Method,
        );

        // The call above names the trait item, which did not change.
        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn row_6_a_signature_change_reaches_dispatch_only() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let method = builder.child(module, "krate::run", SymbolKind::Method);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(
            dispatcher,
            method,
            EdgeKind::MayDispatchTo,
            EdgeDetail::None,
        );
        builder.edge(user, method, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Signature, "krate::run", SymbolKind::Method);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::dispatcher"],
            "a direct call is reached by another rule, not here"
        );
        let step = step_to(&steps, &nothing(), &head, "krate::dispatcher");
        assert_eq!(step.class, ImpactClass::Behaviour);
        assert_eq!(step.certainty, Certainty::Candidate);
        assert_eq!(step.reason, Reason::DispatchesToChanged);
        assert!(step.continues);
    }

    #[test]
    fn row_7_a_declaration_change_reaches_the_users_of_the_type() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let field = builder.child(point, "krate::Point::x", SymbolKind::Field);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        let builder_fn = builder.child(module, "krate::build", SymbolKind::Function);
        let matcher = builder.child(module, "krate::match_it", SymbolKind::Function);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        builder.edge(user, point, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(builder_fn, point, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(matcher, point, EdgeKind::Matches, EdgeDetail::None);
        builder.edge(block, point, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(field, point, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(user, point, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Signature, "krate::Point", SymbolKind::Struct);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec![
                "krate::<impl Trait for Point>",
                "krate::build",
                "krate::match_it",
                "krate::user"
            ],
            "a call edge and the type's own field are not followed"
        );
        assert!(
            steps
                .iter()
                .all(|step| step.class == ImpactClass::Adaptation)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate)
        );
        assert!(steps.iter().all(|step| step.coarse));
        assert!(steps.iter().all(|step| !step.continues));
        assert_eq!(steps[0].reason, Reason::DeclarationChanged);
    }

    #[test]
    fn row_8_a_private_change_reaches_only_referrers_outside_the_module() {
        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let inner = builder.child(root, "krate::inner", SymbolKind::Module);
        let hidden = builder.child(inner, "krate::inner::hidden", SymbolKind::Struct);
        let sibling = builder.child(inner, "krate::inner::sibling", SymbolKind::Function);
        let deep = builder.child(inner, "krate::inner::deep", SymbolKind::Module);
        let nested = builder.child(deep, "krate::inner::deep::user", SymbolKind::Function);
        let outside = builder.child(root, "krate::outside", SymbolKind::Function);
        builder.edge(sibling, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(nested, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(outside, hidden, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::inner::hidden",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Private,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::outside"]
        );
        let step = step_to(&steps, &nothing(), &head, "krate::outside");
        assert_eq!(step.class, ImpactClass::Adaptation);
        assert_eq!(step.reason, Reason::NoLongerVisible);
        assert!(!step.continues);
        assert!(!step.coarse);
    }

    #[test]
    fn row_8_restricted_visibility_is_coarse_and_crate_visibility_crosses_crates() {
        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let inner = builder.child(root, "krate::inner", SymbolKind::Module);
        let deeper = builder.child(inner, "krate::inner::deeper", SymbolKind::Module);
        let hidden = builder.child(deeper, "krate::inner::deeper::hidden", SymbolKind::Struct);
        let sibling = builder.child(
            deeper,
            "krate::inner::deeper::sibling",
            SymbolKind::Function,
        );
        let cousin = builder.child(inner, "krate::inner::cousin", SymbolKind::Function);
        let outside = builder.child(root, "krate::outside", SymbolKind::Function);
        builder.edge(sibling, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(cousin, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(outside, hidden, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::inner::deeper::hidden",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Restricted,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::outside"]
        );
        assert!(steps[0].coarse, "a restricted visibility is a coarse rule");

        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let shared = builder.child(root, "krate::shared", SymbolKind::Struct);
        let neighbour = builder.child(root, "krate::neighbour", SymbolKind::Function);
        let elsewhere = builder.child(root, "other::user", SymbolKind::Function);
        builder.krate(elsewhere, 1);
        builder.edge(neighbour, shared, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(elsewhere, shared, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::shared",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Crate,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["other::user"]
        );
        assert!(!steps[0].coarse);
    }

    #[test]
    fn row_8_a_narrowed_field_reaches_the_literals_and_patterns_that_name_it() {
        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let spec = builder.child(root, "krate::Spec", SymbolKind::Struct);
        let argument = builder.child(spec, "krate::Spec::argument", SymbolKind::Field);
        let name = builder.child(spec, "krate::Spec::name", SymbolKind::Field);
        let parse = builder.child(root, "krate::Spec::parse", SymbolKind::Function);
        let build = builder.add("other::build", SymbolKind::Function);
        let rebuild = builder.add("other::rebuild", SymbolKind::Function);
        let reader = builder.add("other::reader", SymbolKind::Function);
        let pattern = builder.add("other::pattern", SymbolKind::Function);
        let naming = builder.add("other::naming", SymbolKind::Function);
        builder.krate(build, 1);
        builder.krate(rebuild, 1);
        builder.krate(reader, 1);
        builder.krate(pattern, 1);
        builder.krate(naming, 1);
        // A literal carries no edge to the field, only to its container.
        builder.edge(parse, spec, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(build, spec, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(rebuild, spec, EdgeKind::Constructs, EdgeDetail::WithRest);
        builder.edge(reader, argument, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(pattern, argument, EdgeKind::Matches, EdgeDetail::None);
        builder.edge(naming, name, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::Spec::argument",
            SymbolKind::Field,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Crate,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec![
                "other::build",
                "other::pattern",
                "other::reader",
                "other::rebuild"
            ]
        );
        let step = step_to(&steps, &nothing(), &head, "other::build");
        assert_eq!(step.class, ImpactClass::Adaptation);
        assert_eq!(step.reason, Reason::NoLongerVisible);
        assert_eq!(step.certainty, Certainty::Resolved);
        assert!(!step.continues);
        let step = step_to(&steps, &nothing(), &head, "other::rebuild");
        assert_eq!(
            step.certainty,
            Certainty::Candidate,
            "a literal with `..` may leave the field out"
        );
    }

    #[test]
    fn row_8_a_widened_visibility_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let item = builder.child(module, "krate::item", SymbolKind::Struct);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(user, item, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::item",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Private,
                to: Visibility::Public,
            },
        );

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn row_10_a_removal_reaches_base_side_referrers_but_not_the_container() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let gone = builder.child(module, "krate::gone", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, gone, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(module, gone, EdgeKind::Contains, EdgeDetail::None);
        let base = builder.finish();
        let change = change(ChangeKind::Removed, "krate::gone", SymbolKind::Function);
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].side, Side::Base);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].reason, Reason::RefersToRemoved);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_11_a_rename_reaches_base_side_referrers_of_the_previous_name() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let old = builder.child(module, "krate::old_name", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(caller, old, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(other, module, EdgeKind::Calls, EdgeDetail::None);
        let base = builder.finish();
        let change = detailed(
            ChangeKind::Renamed,
            "krate::new_name",
            SymbolKind::Function,
            ChangeDetail::Renamed {
                from: SymbolPath::new("krate::old_name", SymbolKind::Function),
            },
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].reason, Reason::RefersToRenamed);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_12_a_removed_field_skips_readers_of_other_fields() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let x = builder.child(point, "krate::Point::x", SymbolKind::Field);
        let y = builder.child(point, "krate::Point::y", SymbolKind::Field);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let other_reader = builder.child(module, "krate::other_reader", SymbolKind::Function);
        let writer = builder.child(module, "krate::writer", SymbolKind::Function);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        let rest_literal = builder.child(module, "krate::rest_literal", SymbolKind::Function);
        builder.edge(reader, x, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(other_reader, y, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(writer, x, EdgeKind::WritesField, EdgeDetail::None);
        builder.edge(literal, point, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(
            rest_literal,
            point,
            EdgeKind::Constructs,
            EdgeDetail::WithRest,
        );
        let base = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::x",
            SymbolKind::Field,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(
            reached_paths(&steps, &base, &head),
            vec![
                "krate::literal",
                "krate::reader",
                "krate::rest_literal",
                "krate::writer"
            ],
            "a reader of another field is not reached"
        );
        assert!(steps.iter().all(|step| step.side == Side::Base));
        assert_eq!(
            step_to(&steps, &base, &head, "krate::reader").certainty,
            Certainty::Resolved
        );
        assert_eq!(
            step_to(&steps, &base, &head, "krate::rest_literal").certainty,
            Certainty::Candidate,
            "a literal with `..` may not name the field"
        );
        assert!(steps.iter().all(|step| !step.continues));
        assert_eq!(steps[0].reason, Reason::UsesChangedField);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
    }

    #[test]
    fn row_13_a_modified_field_carries_only_reads_further() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let x = builder.child(point, "krate::Point::x", SymbolKind::Field);
        let y = builder.child(point, "krate::Point::y", SymbolKind::Field);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let other_reader = builder.child(module, "krate::other_reader", SymbolKind::Function);
        let writer = builder.child(module, "krate::writer", SymbolKind::Function);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        builder.edge(reader, x, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(other_reader, y, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(writer, x, EdgeKind::WritesField, EdgeDetail::None);
        builder.edge(literal, point, EdgeKind::Constructs, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::x",
            SymbolKind::Field,
            Delta::Modified,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::literal", "krate::reader", "krate::writer"]
        );
        assert!(
            step_to(&steps, &nothing(), &head, "krate::reader").continues,
            "a field read carries behaviour further"
        );
        assert!(!step_to(&steps, &nothing(), &head, "krate::writer").continues);
        assert!(!step_to(&steps, &nothing(), &head, "krate::literal").continues);
    }

    #[test]
    fn row_14_a_new_field_reaches_literals_without_rest() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        let rest_literal = builder.child(module, "krate::rest_literal", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let field = builder.child(point, "krate::Point::x", SymbolKind::Field);
        builder.edge(literal, point, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(
            rest_literal,
            point,
            EdgeKind::Constructs,
            EdgeDetail::WithRest,
        );
        builder.edge(reader, field, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::x",
            SymbolKind::Field,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::literal"]
        );
        assert_eq!(steps[0].reason, Reason::ConstructsWithoutField);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_14_an_invented_field_finds_its_container_by_path() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        let rest_literal = builder.child(module, "krate::rest_literal", SymbolKind::Function);
        builder.edge(literal, point, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(
            rest_literal,
            point,
            EdgeKind::Constructs,
            EdgeDetail::WithRest,
        );
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::<new>",
            SymbolKind::Field,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::literal"]
        );
    }

    #[test]
    fn row_15_a_new_field_reaches_struct_patterns_without_rest() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let pattern = builder.child(module, "krate::pattern", SymbolKind::Function);
        let rest_pattern = builder.child(module, "krate::rest_pattern", SymbolKind::Function);
        let variant = builder.child(module, "krate::Choice::First", SymbolKind::Variant);
        let variant_pattern = builder.child(module, "krate::variant_pattern", SymbolKind::Function);
        builder.edge(pattern, point, EdgeKind::Matches, EdgeDetail::None);
        builder.edge(rest_pattern, point, EdgeKind::Matches, EdgeDetail::WithRest);
        builder.edge(
            variant_pattern,
            variant,
            EdgeKind::Matches,
            EdgeDetail::None,
        );
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::<new>",
            SymbolKind::Field,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::pattern"]
        );
        assert_eq!(steps[0].reason, Reason::PatternMissesField);

        let variant_change = member(
            ChangeKind::Field,
            "krate::Choice::First::<new>",
            SymbolKind::Field,
            Delta::Added,
        );
        assert!(
            first(&nothing(), &head, &variant_change).is_empty(),
            "a field of a variant is not a struct pattern"
        );
    }

    #[test]
    fn row_16_an_exhaustive_match_is_an_adaptation_and_a_wildcard_is_not() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let exhaustive = builder.child(module, "krate::exhaustive", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            exhaustive,
            choice,
            EdgeKind::Matches,
            EdgeDetail::MatchExhaustive,
        );
        builder.edge(other, choice, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::<new>",
            SymbolKind::Variant,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::exhaustive"],
            "a wildcard match is reached by another rule, not here"
        );
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].reason, Reason::ExhaustiveMatchMissesVariant);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_17_a_wildcard_match_is_behaviour_and_continues() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let wildcard = builder.child(module, "krate::wildcard", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            wildcard,
            choice,
            EdgeKind::Matches,
            EdgeDetail::MatchWildcard,
        );
        builder.edge(other, choice, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::<new>",
            SymbolKind::Variant,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::wildcard"],
            "an exhaustive match is reached by another rule, not here"
        );
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert_eq!(steps[0].reason, Reason::WildcardCatchesNewVariant);
        assert!(steps[0].continues);
    }

    #[test]
    fn row_18_a_removed_variant_reaches_its_uses_on_the_base_side() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let first_variant = builder.child(choice, "krate::Choice::First", SymbolKind::Variant);
        let payload = builder.child(
            first_variant,
            "krate::Choice::First::value",
            SymbolKind::Field,
        );
        let second = builder.child(choice, "krate::Choice::Second", SymbolKind::Variant);
        let constructor = builder.child(module, "krate::constructor", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            constructor,
            first_variant,
            EdgeKind::Constructs,
            EdgeDetail::None,
        );
        builder.edge(reader, payload, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(other, second, EdgeKind::Matches, EdgeDetail::None);
        let base = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::First",
            SymbolKind::Variant,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(
            reached_paths(&steps, &base, &head),
            vec!["krate::constructor", "krate::reader"],
            "a use of another variant is not reached"
        );
        assert!(steps.iter().all(|step| step.side == Side::Base));
        assert_eq!(steps[0].reason, Reason::UsesChangedVariant);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_19_a_reshaped_variant_reaches_its_uses() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let first_variant = builder.child(choice, "krate::Choice::First", SymbolKind::Variant);
        let constructor = builder.child(module, "krate::constructor", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            constructor,
            first_variant,
            EdgeKind::Constructs,
            EdgeDetail::None,
        );
        builder.edge(other, choice, EdgeKind::Matches, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::First",
            SymbolKind::Variant,
            Delta::Modified,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::constructor"]
        );
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].reason, Reason::UsesChangedVariant);
    }

    #[test]
    fn row_20_a_required_trait_item_reaches_the_implementations() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        builder.child(trait_, "krate::Trait::extra", SymbolKind::Method);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(point, trait_, EdgeKind::Implements, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::extra",
            SymbolKind::Method,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::<impl Trait for Point>"],
            "only an implementation block is reached"
        );
        assert_eq!(steps[0].reason, Reason::MustImplementTraitItem);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_21_a_trait_item_with_a_body_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let default = builder.child(trait_, "krate::Trait::extra", SymbolKind::Method);
        builder.body(default);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::extra",
            SymbolKind::Method,
            Delta::Added,
        );

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn row_22_a_modified_trait_item_reaches_implementations_and_callers() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        let implementation = builder.child(
            block,
            "krate::<impl Trait for Point>::run",
            SymbolKind::Method,
        );
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(implementation, run, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, run, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(user, trait_, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            Delta::Modified,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec![
                "krate::<impl Trait for Point>::run",
                "krate::caller",
                "krate::holder"
            ]
        );
        let implementation = step_to(
            &steps,
            &nothing(),
            &head,
            "krate::<impl Trait for Point>::run",
        );
        assert_eq!(implementation.reason, Reason::MustImplementTraitItem);
        assert!(!implementation.continues);
        let caller = step_to(&steps, &nothing(), &head, "krate::caller");
        assert_eq!(caller.reason, Reason::UsesChangedTraitItem);
        assert!(caller.continues);
    }

    #[test]
    fn row_23_a_removed_trait_item_reaches_its_uses_on_the_base_side() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(user, trait_, EdgeKind::UsesType, EdgeDetail::None);
        let base = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].reason, Reason::UsesChangedTraitItem);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_24_a_changed_default_body_skips_implementations_that_override_it() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        builder.body(run);
        let overriding = builder.child(module, "krate::<impl Trait for A>", SymbolKind::Impl);
        let overridden = builder.child(
            overriding,
            "krate::<impl Trait for A>::run",
            SymbolKind::Method,
        );
        let inheriting = builder.child(module, "krate::<impl Trait for B>", SymbolKind::Impl);
        builder.child(
            inheriting,
            "krate::<impl Trait for B>::other",
            SymbolKind::Method,
        );
        builder.edge(overriding, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(inheriting, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(overridden, run, EdgeKind::Implements, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            ChangeDetail::DefaultBody,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::<impl Trait for B>"],
            "an implementation that overrides the default is not reached"
        );
        assert_eq!(steps[0].reason, Reason::InheritsChangedDefault);
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_25_a_call_to_a_default_body_is_certain_unless_an_impl_overrides_it() {
        let build = |overriding: bool| {
            let mut builder = GraphBuilder::new();
            let module = builder.add("krate", SymbolKind::Module);
            let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
            let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
            builder.body(run);
            let block = builder.child(module, "krate::<impl Trait for A>", SymbolKind::Impl);
            let caller = builder.child(module, "krate::caller", SymbolKind::Function);
            builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
            if overriding {
                let overridden =
                    builder.child(block, "krate::<impl Trait for A>::run", SymbolKind::Method);
                builder.edge(overridden, run, EdgeKind::Implements, EdgeDetail::None);
            }
            builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
            builder.finish()
        };
        let change = detailed(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            ChangeDetail::DefaultBody,
        );

        let plain = build(false);
        let steps = first(&nothing(), &plain, &change);
        let caller = step_to(&steps, &nothing(), &plain, "krate::caller");
        assert_eq!(caller.certainty, Certainty::Resolved);
        assert_eq!(caller.reason, Reason::CallsChanged);
        assert!(caller.continues);

        let overriding = build(true);
        let steps = first(&nothing(), &overriding, &change);
        let caller = step_to(&steps, &nothing(), &overriding, "krate::caller");
        assert_eq!(
            caller.certainty,
            Certainty::Candidate,
            "an implementation may replace the default"
        );
    }

    #[test]
    fn row_26_a_default_body_held_as_a_value_is_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        builder.body(run);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(holder, run, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(reader, run, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            ChangeDetail::DefaultBody,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::holder"],
            "a call is reached by another rule, not here"
        );
        assert_eq!(steps[0].certainty, Certainty::Candidate);
        assert_eq!(steps[0].reason, Reason::TakesChangedAsValue);
    }

    #[test]
    fn row_27_a_new_implementation_reaches_the_users_of_its_items() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let block = builder.child(module, "krate::<impl Point>", SymbolKind::Impl);
        let run = builder.child(block, "krate::<impl Point>::run", SymbolKind::Method);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(dispatcher, run, EdgeKind::MayDispatchTo, EdgeDetail::None);
        builder.edge(user, block, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Impl,
            "krate::<impl Point>",
            SymbolKind::Impl,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller", "krate::dispatcher"],
            "a use of the impl block itself is not followed"
        );
        assert_eq!(steps[0].reason, Reason::ImplResolutionChanged);
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert!(steps[0].continues);
    }

    #[test]
    fn row_28_a_removed_implementation_reaches_its_users_on_the_base_side() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let block = builder.child(module, "krate::<impl Point>", SymbolKind::Impl);
        let run = builder.child(block, "krate::<impl Point>::run", SymbolKind::Method);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(user, block, EdgeKind::UsesType, EdgeDetail::None);
        let base = builder.finish();
        let change = member(
            ChangeKind::Impl,
            "krate::<impl Point>",
            SymbolKind::Impl,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].reason, Reason::ImplResolutionChanged);
        assert!(steps[0].continues);
    }

    #[test]
    fn row_29_a_changed_value_reaches_its_readers() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let limit = builder.child(module, "krate::LIMIT", SymbolKind::Const);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(reader, limit, EdgeKind::UsesValue, EdgeDetail::None);
        builder.edge(user, limit, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::ConstValue, "krate::LIMIT", SymbolKind::Const);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::reader"]
        );
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert_eq!(steps[0].reason, Reason::UsesChangedValue);
        assert!(steps[0].continues);
    }

    #[test]
    fn row_30_a_retyped_value_makes_its_readers_adapt() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let limit = builder.child(module, "krate::LIMIT", SymbolKind::Static);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(reader, limit, EdgeKind::UsesValue, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::ConstValue,
            "krate::LIMIT",
            SymbolKind::Static,
            ChangeDetail::Retyped,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::reader"]
        );
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].reason, Reason::UsesRetypedValue);
        assert!(steps[0].continues);
    }

    #[test]
    fn row_31_a_changed_alias_reaches_its_users_as_a_coarse_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let alias = builder.child(module, "krate::Alias", SymbolKind::TypeAlias);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(user, alias, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(reader, alias, EdgeKind::UsesValue, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::TypeAlias, "krate::Alias", SymbolKind::TypeAlias);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::user"]
        );
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].certainty, Certainty::Candidate);
        assert_eq!(steps[0].reason, Reason::UsesChangedAlias);
        assert!(steps[0].coarse);
        assert!(!steps[0].continues);
    }

    #[test]
    fn row_32_a_changed_macro_reaches_its_invocations() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let macro_ = builder.child(module, "krate::declare", SymbolKind::Macro);
        let invoker = builder.child(module, "krate::invoker", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(invoker, macro_, EdgeKind::Expands, EdgeDetail::None);
        builder.edge(caller, invoker, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::MacroDef, "krate::declare", SymbolKind::Macro);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::invoker"]
        );
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert_eq!(steps[0].certainty, Certainty::Candidate);
        assert_eq!(steps[0].reason, Reason::InvokesChangedMacro);
        assert!(steps[0].continues);
    }

    #[test]
    fn row_33_a_changed_attribute_reaches_what_the_body_rules_reach_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let datatype = builder.child(module, "krate::Type", SymbolKind::Struct);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(datatype, function, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Attribute, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller", "krate::holder"]
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.reason == Reason::AttributeChanged)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.class == ImpactClass::Behaviour)
        );
        assert!(steps.iter().all(|step| step.continues));
    }

    #[test]
    fn row_34_an_attribute_on_anything_but_a_function_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(user, point, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Attribute, "krate::Point", SymbolKind::Struct);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_formatting_only_change_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::NonSemantic, "krate::f", SymbolKind::Function);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_change_to_a_symbol_the_side_does_not_hold_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::gone", SymbolKind::Function);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_chain_does_not_reach_through_the_target_itself() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let inner = builder.child(function, "krate::f::inner", SymbolKind::Function);
        builder.edge(inner, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Function);

        assert!(
            first(&nothing(), &head, &change).is_empty(),
            "a reference from inside the target is not followed"
        );
    }

    #[test]
    fn behaviour_propagation_follows_calls_and_values_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let constant = builder.child(module, "krate::LIMIT", SymbolKind::Const);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(
            dispatcher,
            function,
            EdgeKind::MayDispatchTo,
            EdgeDetail::None,
        );
        builder.edge(reader, constant, EdgeKind::UsesValue, EdgeDetail::None);
        builder.edge(user, function, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let base = nothing();
        let graphs = Graphs::new(&base, &head);

        let mut reached = super::onward(function, &graphs);
        reached.extend(super::onward(constant, &graphs));

        let mut paths = reached_paths(&reached, &base, &head);
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "krate::caller",
                "krate::dispatcher",
                "krate::holder",
                "krate::reader"
            ]
        );
        assert!(
            reached
                .iter()
                .all(|step| step.class == ImpactClass::Behaviour)
        );
        assert!(reached.iter().all(|step| step.continues));
        assert_eq!(
            step_to(&reached, &base, &head, "krate::caller").certainty,
            Certainty::Resolved
        );
        let candidate = step_to(&reached, &base, &head, "krate::dispatcher");
        assert_eq!(candidate.certainty, Certainty::Candidate);
        assert_eq!(candidate.reason, Reason::DispatchesToChanged);
    }

    #[test]
    fn behaviour_propagation_follows_an_invocation_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let derive = builder.child(module, "krate::derive_name", SymbolKind::Function);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let helper = builder.child(module, "krate::helper", SymbolKind::Function);
        builder.edge(point, derive, EdgeKind::Expands, EdgeDetail::None);
        builder.edge(helper, derive, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let base = nothing();
        let graphs = Graphs::new(&base, &head);

        let reached = super::onward(derive, &graphs);

        assert_eq!(
            reached_paths(&reached, &base, &head),
            vec!["krate::Point", "krate::helper"]
        );
        let expansion = step_to(&reached, &base, &head, "krate::Point");
        assert_eq!(expansion.class, ImpactClass::Behaviour);
        assert_eq!(expansion.certainty, Certainty::Candidate);
        assert_eq!(expansion.reason, Reason::InvokesChangedMacro);
        assert!(expansion.continues);
        assert!(!expansion.coarse);
    }

    #[test]
    fn the_table_holds_every_row_that_follows_edges() {
        assert_eq!(
            super::RULES.len(),
            31,
            "one entry for every kind of change that follows edges"
        );
    }
}
