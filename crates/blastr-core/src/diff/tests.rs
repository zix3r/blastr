//! Tests for [`classify`](super::classify).

use super::testkit::{GraphBuilder, added, deleted, modified, renamed_file};
use super::*;
use blastr_model::Visibility;

/// Classifies with the default parameters.
fn classify_in(base: RevisionGraph, head: RevisionGraph, changed: &[ChangedFile]) -> Vec<Change> {
    classify(&base, &head, changed, &MatchParams::default())
}

/// The changes as `"<kind> <path> <detail>"` lines, detail omitted when empty.
fn summary(changes: &[Change]) -> Vec<String> {
    changes.iter().map(text).collect()
}

fn text(change: &Change) -> String {
    let detail = match &change.detail {
        ChangeDetail::None => String::new(),
        ChangeDetail::Member(delta) => format!("{delta:?}").to_lowercase(),
        ChangeDetail::DefaultBody => "default-body".to_owned(),
        ChangeDetail::Renamed { from } => format!("from {}", from.path),
        ChangeDetail::Visibility { from, to } => format!("{from:?}->{to:?}"),
        ChangeDetail::Manifest(description) => description.clone(),
    };
    format!("{} {} {detail}", change.kind, change.symbol.path)
        .trim_end()
        .to_owned()
}

#[test]
fn reports_a_body_change() {
    let mut base = GraphBuilder::new();
    base.add("krate::total", SymbolKind::Function, "src/lib.rs", 0, 40)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::total", SymbolKind::Function, "src/lib.rs", 0, 44)
        .hashes(11, 22);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["body krate::total"]);
}

#[test]
fn a_signature_change_does_not_report_a_body() {
    let mut base = GraphBuilder::new();
    base.add("krate::f", SymbolKind::Function, "src/lib.rs", 0, 30)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::f", SymbolKind::Function, "src/lib.rs", 0, 30)
        .hashes(12, 22);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["signature krate::f"]);
}

#[test]
fn reports_a_visibility_change() {
    let mut base = GraphBuilder::new();
    base.add("krate::f", SymbolKind::Function, "src/lib.rs", 0, 20)
        .hashes(11, 21)
        .vis(Visibility::Private);
    let mut head = GraphBuilder::new();
    head.add("krate::f", SymbolKind::Function, "src/lib.rs", 0, 20)
        .hashes(11, 21)
        .vis(Visibility::Public);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["visibility krate::f Private->Public"]);
}

#[test]
fn reports_an_attribute_change() {
    let mut base = GraphBuilder::new();
    base.add("krate::f", SymbolKind::Function, "src/lib.rs", 0, 20)
        .hashes(11, 21)
        .attr(0);
    let mut head = GraphBuilder::new();
    head.add("krate::f", SymbolKind::Function, "src/lib.rs", 0, 20)
        .hashes(11, 21)
        .attr(5);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["attribute krate::f"]);
}

#[test]
fn reports_a_removal_and_an_addition() {
    let mut base = GraphBuilder::new();
    base.add("krate::gone", SymbolKind::Function, "src/lib.rs", 0, 20)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::fresh", SymbolKind::Function, "src/lib.rs", 30, 50)
        .hashes(12, 22);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        ["removed krate::gone", "added krate::fresh"]
    );
}

#[test]
fn reports_symbols_of_an_added_file() {
    let mut head = GraphBuilder::new();
    head.add("krate::fresh", SymbolKind::Function, "src/new.rs", 0, 20)
        .hashes(12, 22);
    let changes = classify_in(
        RevisionGraph::default(),
        head.finish(),
        &[added("src/new.rs")],
    );
    assert_eq!(summary(&changes), ["added krate::fresh"]);
}

#[test]
fn reports_fields_added_removed_and_retyped() {
    let mut base = GraphBuilder::new();
    let old = base
        .add("krate::S", SymbolKind::Struct, "src/lib.rs", 0, 60)
        .sig(7)
        .id();
    base.add("krate::S::x", SymbolKind::Field, "src/lib.rs", 10, 20)
        .sig(11)
        .parent(old);
    base.add("krate::S::y", SymbolKind::Field, "src/lib.rs", 21, 31)
        .sig(12)
        .parent(old);
    let mut head = GraphBuilder::new();
    let new = head
        .add("krate::S", SymbolKind::Struct, "src/lib.rs", 0, 60)
        .sig(7)
        .id();
    head.add("krate::S::x", SymbolKind::Field, "src/lib.rs", 10, 20)
        .sig(13)
        .parent(new);
    head.add("krate::S::z", SymbolKind::Field, "src/lib.rs", 21, 31)
        .sig(12)
        .parent(new);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        [
            "field krate::S::x modified",
            "field krate::S::y removed",
            "field krate::S::z added",
        ]
    );
}

#[test]
fn reports_variants_but_not_the_fields_of_a_removed_one() {
    let mut base = GraphBuilder::new();
    let old = base
        .add("krate::E", SymbolKind::Enum, "src/lib.rs", 0, 150)
        .sig(3)
        .id();
    base.add("krate::E::A", SymbolKind::Variant, "src/lib.rs", 10, 20)
        .sig(21)
        .parent(old);
    let removed = base
        .add("krate::E::B", SymbolKind::Variant, "src/lib.rs", 30, 70)
        .sig(22)
        .parent(old)
        .id();
    base.add("krate::E::B::x", SymbolKind::Field, "src/lib.rs", 40, 50)
        .sig(31)
        .parent(removed);
    base.add("krate::E::B::y", SymbolKind::Field, "src/lib.rs", 51, 61)
        .sig(32)
        .parent(removed);
    base.add("krate::E::C", SymbolKind::Variant, "src/lib.rs", 80, 90)
        .sig(23)
        .parent(old);
    let mut head = GraphBuilder::new();
    let new = head
        .add("krate::E", SymbolKind::Enum, "src/lib.rs", 0, 150)
        .sig(3)
        .id();
    head.add("krate::E::A", SymbolKind::Variant, "src/lib.rs", 10, 20)
        .sig(24)
        .parent(new);
    head.add("krate::E::D", SymbolKind::Variant, "src/lib.rs", 30, 70)
        .sig(22)
        .parent(new);
    head.add("krate::E::C", SymbolKind::Variant, "src/lib.rs", 80, 90)
        .sig(23)
        .parent(new);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        [
            "variant krate::E::A modified",
            "variant krate::E::B removed",
            "variant krate::E::D added",
        ]
    );
}

#[test]
fn reports_trait_items() {
    let mut base = GraphBuilder::new();
    let old = base
        .add("krate::T", SymbolKind::Trait, "src/lib.rs", 0, 200)
        .sig(1)
        .id();
    base.add("krate::T::kept", SymbolKind::Method, "src/lib.rs", 10, 40)
        .hashes(20, 30)
        .in_trait()
        .parent(old);
    base.add("krate::T::gone", SymbolKind::Method, "src/lib.rs", 50, 80)
        .hashes(21, 0)
        .in_trait()
        .parent(old);
    base.add(
        "krate::T::reshaped",
        SymbolKind::Method,
        "src/lib.rs",
        90,
        120,
    )
    .hashes(22, 0)
    .in_trait()
    .parent(old);
    let mut head = GraphBuilder::new();
    let new = head
        .add("krate::T", SymbolKind::Trait, "src/lib.rs", 0, 200)
        .sig(1)
        .id();
    head.add("krate::T::kept", SymbolKind::Method, "src/lib.rs", 10, 40)
        .hashes(20, 31)
        .in_trait()
        .parent(new);
    head.add(
        "krate::T::reshaped",
        SymbolKind::Method,
        "src/lib.rs",
        90,
        120,
    )
    .hashes(23, 0)
    .in_trait()
    .parent(new);
    head.add(
        "krate::T::fresh",
        SymbolKind::Method,
        "src/lib.rs",
        130,
        160,
    )
    .hashes(24, 0)
    .in_trait()
    .parent(new);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        [
            "trait-item krate::T::kept default-body",
            "trait-item krate::T::gone removed",
            "trait-item krate::T::reshaped modified",
            "trait-item krate::T::fresh added",
        ]
    );
}

#[test]
fn reports_impls_without_their_items() {
    let mut base = GraphBuilder::new();
    base.add("krate::Point", SymbolKind::Struct, "src/lib.rs", 0, 20)
        .sig(1);
    let old = base
        .add(
            "krate::<impl Point>",
            SymbolKind::Impl,
            "src/lib.rs",
            25,
            60,
        )
        .sig(2)
        .id();
    base.add(
        "krate::Point::norm",
        SymbolKind::Method,
        "src/lib.rs",
        30,
        55,
    )
    .hashes(3, 4)
    .parent(old);
    let mut head = GraphBuilder::new();
    head.add("krate::Point", SymbolKind::Struct, "src/lib.rs", 0, 20)
        .sig(1);
    let new = head
        .add(
            "krate::<impl Display for Point>",
            SymbolKind::Impl,
            "src/lib.rs",
            25,
            70,
        )
        .sig(5)
        .id();
    head.add(
        "krate::<impl Display for Point>::fmt",
        SymbolKind::Method,
        "src/lib.rs",
        30,
        65,
    )
    .hashes(6, 7)
    .parent(new);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        [
            "impl krate::<impl Display for Point> added",
            "impl krate::<impl Point> removed",
        ]
    );
}

#[test]
fn an_added_or_removed_struct_covers_its_fields() {
    let mut head = GraphBuilder::new();
    let new = head
        .add("krate::S", SymbolKind::Struct, "src/lib.rs", 0, 40)
        .sig(7)
        .id();
    head.add("krate::S::x", SymbolKind::Field, "src/lib.rs", 10, 20)
        .sig(11)
        .parent(new);
    head.add("krate::S::y", SymbolKind::Field, "src/lib.rs", 21, 31)
        .sig(12)
        .parent(new);
    let mut base = GraphBuilder::new();
    let old = base
        .add("krate::Gone", SymbolKind::Struct, "src/lib.rs", 0, 40)
        .sig(9)
        .id();
    base.add("krate::Gone::x", SymbolKind::Field, "src/lib.rs", 10, 20)
        .sig(11)
        .parent(old);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["added krate::S", "removed krate::Gone"]);
}

#[test]
fn reports_const_value_changes() {
    let mut base = GraphBuilder::new();
    base.add("krate::LIMIT", SymbolKind::Const, "src/lib.rs", 0, 20)
        .hashes(11, 21);
    base.add("krate::OTHER", SymbolKind::Static, "src/lib.rs", 30, 50)
        .hashes(12, 22);
    let mut head = GraphBuilder::new();
    head.add("krate::LIMIT", SymbolKind::Const, "src/lib.rs", 0, 20)
        .hashes(11, 23);
    head.add("krate::OTHER", SymbolKind::Static, "src/lib.rs", 30, 50)
        .sig(13)
        .body(22);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        ["const-value krate::LIMIT", "const-value krate::OTHER"]
    );
}

#[test]
fn reports_a_type_alias_change() {
    let mut base = GraphBuilder::new();
    base.add("krate::Alias", SymbolKind::TypeAlias, "src/lib.rs", 0, 20)
        .sig(11);
    let mut head = GraphBuilder::new();
    head.add("krate::Alias", SymbolKind::TypeAlias, "src/lib.rs", 0, 20)
        .sig(12);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["type-alias krate::Alias"]);
}

#[test]
fn reports_a_macro_definition_change() {
    let mut base = GraphBuilder::new();
    base.add("krate::m", SymbolKind::Macro, "src/lib.rs", 0, 20)
        .body(21);
    let mut head = GraphBuilder::new();
    head.add("krate::m", SymbolKind::Macro, "src/lib.rs", 0, 20)
        .body(22);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["macro-def krate::m"]);
}

#[test]
fn a_reshaped_variant_covers_its_added_and_removed_fields() {
    // `Circle(f64)` becomes `Circle { radius: f64 }`.
    let mut base = GraphBuilder::new();
    let old = base
        .add("krate::Shape", SymbolKind::Enum, "src/lib.rs", 0, 80)
        .sig(3)
        .id();
    let circle = base
        .add(
            "krate::Shape::Circle",
            SymbolKind::Variant,
            "src/lib.rs",
            20,
            31,
        )
        .sig(21)
        .parent(old)
        .id();
    base.add(
        "krate::Shape::Circle::0",
        SymbolKind::Field,
        "src/lib.rs",
        27,
        30,
    )
    .sig(31)
    .parent(circle);
    let mut head = GraphBuilder::new();
    let new = head
        .add("krate::Shape", SymbolKind::Enum, "src/lib.rs", 0, 90)
        .sig(3)
        .id();
    let circle = head
        .add(
            "krate::Shape::Circle",
            SymbolKind::Variant,
            "src/lib.rs",
            20,
            42,
        )
        .sig(22)
        .parent(new)
        .id();
    head.add(
        "krate::Shape::Circle::radius",
        SymbolKind::Field,
        "src/lib.rs",
        29,
        40,
    )
    .sig(31)
    .parent(circle);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["variant krate::Shape::Circle modified"]);
}

#[test]
fn a_reshaped_variant_covers_its_matched_fields() {
    // `Rect { w: f64 }` becomes `Rect { w: u32, h: u32 }`.
    let mut base = GraphBuilder::new();
    let rect = base
        .add(
            "krate::Shape::Rect",
            SymbolKind::Variant,
            "src/lib.rs",
            20,
            40,
        )
        .sig(21)
        .id();
    base.add(
        "krate::Shape::Rect::w",
        SymbolKind::Field,
        "src/lib.rs",
        27,
        33,
    )
    .sig(31)
    .parent(rect);
    let mut head = GraphBuilder::new();
    let rect = head
        .add(
            "krate::Shape::Rect",
            SymbolKind::Variant,
            "src/lib.rs",
            20,
            50,
        )
        .sig(22)
        .id();
    head.add(
        "krate::Shape::Rect::w",
        SymbolKind::Field,
        "src/lib.rs",
        27,
        33,
    )
    .sig(32)
    .parent(rect);
    head.add(
        "krate::Shape::Rect::h",
        SymbolKind::Field,
        "src/lib.rs",
        35,
        41,
    )
    .sig(32)
    .parent(rect);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["variant krate::Shape::Rect modified"]);
}

#[test]
fn reports_a_pure_rename_as_a_rename_only() {
    let mut base = GraphBuilder::new();
    base.add("krate::a::f", SymbolKind::Function, "src/a.rs", 0, 30)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::a::g", SymbolKind::Function, "src/a.rs", 0, 30)
        .hashes(11, 21);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/a.rs")]);
    assert_eq!(summary(&changes), ["renamed krate::a::g from krate::a::f"]);
}

#[test]
fn a_rename_with_one_statement_changed_is_a_rename_and_a_body_change() {
    // About twelve statements, one of them edited: most 3-grams survive.
    let before: Vec<u32> = (1..=16).collect();
    let mut after: Vec<u32> = (1..=13).collect();
    after.extend([20, 21, 22]);
    let mut base = GraphBuilder::new();
    base.add("krate::total", SymbolKind::Function, "src/lib.rs", 0, 300)
        .hashes(11, 21)
        .sketch(&before);
    let mut head = GraphBuilder::new();
    head.add("krate::tally", SymbolKind::Function, "src/lib.rs", 0, 304)
        .hashes(11, 22)
        .sketch(&after);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        [
            "body krate::tally",
            "renamed krate::tally from krate::total"
        ]
    );
}

#[test]
fn a_rename_with_a_signature_change_is_a_rename_and_a_signature_change() {
    let mut base = GraphBuilder::new();
    base.add("krate::f", SymbolKind::Function, "src/lib.rs", 0, 30)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::g", SymbolKind::Function, "src/lib.rs", 0, 34)
        .hashes(12, 21);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        ["signature krate::g", "renamed krate::g from krate::f"]
    );
}

#[test]
fn a_renamed_macro_is_recognised_by_its_rules() {
    let mut base = GraphBuilder::new();
    base.add("krate::old", SymbolKind::Macro, "src/lib.rs", 0, 30)
        .body(21)
        .sketch(&[1, 2, 3, 4, 5]);
    base.add("krate::unrelated", SymbolKind::Macro, "src/lib.rs", 40, 60)
        .body(23)
        .sketch(&[50, 60]);
    let mut head = GraphBuilder::new();
    head.add("krate::new", SymbolKind::Macro, "src/lib.rs", 0, 32)
        .body(22)
        .sketch(&[1, 2, 3, 4, 9]);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        [
            "renamed krate::new from krate::old",
            "macro-def krate::new",
            "removed krate::unrelated",
        ]
    );
}

#[test]
fn a_renamed_type_alias_is_paired_when_its_target_is_unique() {
    let mut base = GraphBuilder::new();
    base.add("krate::Old", SymbolKind::TypeAlias, "src/lib.rs", 0, 20)
        .sig(11);
    let mut head = GraphBuilder::new();
    head.add("krate::New", SymbolKind::TypeAlias, "src/lib.rs", 0, 20)
        .sig(11);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["renamed krate::New from krate::Old"]);
}

#[test]
fn renamed_type_aliases_with_one_target_are_not_guessed() {
    let mut base = GraphBuilder::new();
    base.add("krate::A", SymbolKind::TypeAlias, "src/lib.rs", 0, 20)
        .sig(11);
    base.add("krate::B", SymbolKind::TypeAlias, "src/lib.rs", 30, 50)
        .sig(11);
    let mut head = GraphBuilder::new();
    head.add("krate::C", SymbolKind::TypeAlias, "src/lib.rs", 60, 80)
        .sig(11);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        ["removed krate::A", "removed krate::B", "added krate::C"]
    );
}

#[test]
fn a_struct_with_other_members_is_not_a_rename() {
    let mut base = GraphBuilder::new();
    let old = base
        .add("krate::Old", SymbolKind::Struct, "src/lib.rs", 0, 30)
        .sig(5)
        .id();
    base.add("krate::Old::x", SymbolKind::Field, "src/lib.rs", 10, 20)
        .sig(11)
        .parent(old);
    let mut head = GraphBuilder::new();
    let new = head
        .add("krate::New", SymbolKind::Struct, "src/lib.rs", 40, 70)
        .sig(5)
        .id();
    head.add("krate::New::y", SymbolKind::Field, "src/lib.rs", 50, 60)
        .sig(11)
        .parent(new);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        ["removed krate::Old", "added krate::New"]
    );
}

#[test]
fn a_move_with_a_small_body_edit_is_a_rename_and_a_body_change() {
    let mut base = GraphBuilder::new();
    base.add("krate::a::f", SymbolKind::Function, "src/a.rs", 0, 30)
        .hashes(11, 21)
        .sketch(&[1, 2, 3, 4]);
    let mut head = GraphBuilder::new();
    head.add("krate::b::f", SymbolKind::Function, "src/b.rs", 0, 34)
        .hashes(11, 22)
        .sketch(&[1, 2, 3, 4]);
    let changes = classify_in(
        base.finish(),
        head.finish(),
        &[modified("src/a.rs"), modified("src/b.rs")],
    );
    assert_eq!(
        summary(&changes),
        ["body krate::b::f", "renamed krate::b::f from krate::a::f"]
    );
}

#[test]
fn a_move_below_the_similarity_threshold_is_a_removal_and_an_addition() {
    let mut base = GraphBuilder::new();
    base.add("krate::a::f", SymbolKind::Function, "src/a.rs", 0, 30)
        .hashes(11, 21)
        .sketch(&[1, 2, 3, 4]);
    let mut head = GraphBuilder::new();
    head.add("krate::b::f", SymbolKind::Function, "src/b.rs", 0, 34)
        .hashes(11, 22)
        .sketch(&[9, 8, 7, 6]);
    let changes = classify_in(
        base.finish(),
        head.finish(),
        &[modified("src/a.rs"), modified("src/b.rs")],
    );
    assert_eq!(
        summary(&changes),
        ["removed krate::a::f", "added krate::b::f"]
    );
}

#[test]
fn a_renamed_struct_keeps_its_fields() {
    let mut base = GraphBuilder::new();
    let old = base
        .add("krate::Old", SymbolKind::Struct, "src/lib.rs", 0, 60)
        .sig(5)
        .id();
    base.add("krate::Old::x", SymbolKind::Field, "src/lib.rs", 10, 20)
        .sig(11)
        .parent(old);
    base.add("krate::Old::y", SymbolKind::Field, "src/lib.rs", 21, 31)
        .sig(12)
        .parent(old);
    let mut head = GraphBuilder::new();
    let new = head
        .add("krate::New", SymbolKind::Struct, "src/lib.rs", 0, 60)
        .sig(5)
        .id();
    head.add("krate::New::x", SymbolKind::Field, "src/lib.rs", 10, 20)
        .sig(11)
        .parent(new);
    head.add("krate::New::y", SymbolKind::Field, "src/lib.rs", 21, 31)
        .sig(12)
        .parent(new);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["renamed krate::New from krate::Old"]);
}

#[test]
fn reports_a_function_moved_between_changed_files() {
    let mut base = GraphBuilder::new();
    base.add("krate::a::f", SymbolKind::Function, "src/a.rs", 0, 30)
        .hashes(3, 4);
    let mut head = GraphBuilder::new();
    head.add("krate::b::f", SymbolKind::Function, "src/b.rs", 0, 30)
        .hashes(3, 4);
    let changes = classify_in(
        base.finish(),
        head.finish(),
        &[modified("src/a.rs"), modified("src/b.rs")],
    );
    assert_eq!(summary(&changes), ["renamed krate::b::f from krate::a::f"]);
}

#[test]
fn ignores_symbols_of_files_that_are_not_changed() {
    let mut base = GraphBuilder::new();
    base.add("krate::other", SymbolKind::Function, "src/other.rs", 0, 20)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::other", SymbolKind::Function, "src/other.rs", 0, 20)
        .hashes(12, 22);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert!(changes.is_empty(), "{changes:?}");
}

#[test]
fn reports_one_non_semantic_change_for_a_formatting_only_file() {
    let mut base = GraphBuilder::new();
    base.add("krate", SymbolKind::Module, "src/lib.rs", 0, 100)
        .sig(0);
    base.add("krate::f", SymbolKind::Function, "src/lib.rs", 10, 50)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate", SymbolKind::Module, "src/lib.rs", 0, 100)
        .sig(0);
    head.add("krate::f", SymbolKind::Function, "src/lib.rs", 10, 50)
        .hashes(11, 21);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["non-semantic krate"]);
    assert_eq!(changes[0].old, None);
    assert_eq!(
        changes[0]
            .new
            .as_ref()
            .map(|location| location.file.as_str()),
        Some("src/lib.rs")
    );
}

#[test]
fn a_formatting_only_file_without_a_module_uses_its_first_symbol() {
    let mut base = GraphBuilder::new();
    base.add("krate::f", SymbolKind::Function, "src/a.rs", 0, 4)
        .hashes(12, 22);
    base.add("krate::g", SymbolKind::Function, "src/a.rs", 5, 30)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::f", SymbolKind::Function, "src/a.rs", 0, 4)
        .hashes(12, 22);
    head.add("krate::g", SymbolKind::Function, "src/a.rs", 5, 30)
        .hashes(11, 21);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/a.rs")]);
    assert_eq!(summary(&changes), ["non-semantic krate::f"]);
}

#[test]
fn never_reports_modules() {
    let mut base = GraphBuilder::new();
    base.add("krate::m", SymbolKind::Module, "src/m.rs", 0, 50);
    let changes = classify_in(
        base.finish(),
        RevisionGraph::default(),
        &[deleted("src/m.rs")],
    );
    assert!(changes.is_empty(), "{changes:?}");
}

#[test]
fn macro_generated_symbols_take_part_like_any_other() {
    let mut base = GraphBuilder::new();
    base.add("krate::gen", SymbolKind::Function, "src/lib.rs", 0, 20)
        .hashes(11, 21)
        .macro_generated();
    let mut head = GraphBuilder::new();
    head.add("krate::gen", SymbolKind::Function, "src/lib.rs", 0, 24)
        .hashes(11, 22)
        .macro_generated();
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(summary(&changes), ["body krate::gen"]);
}

#[test]
fn limits_the_base_side_to_the_old_path_of_a_renamed_file() {
    let mut base = GraphBuilder::new();
    base.add("krate::f", SymbolKind::Function, "src/old.rs", 0, 20)
        .hashes(11, 21);
    let mut head = GraphBuilder::new();
    head.add("krate::f", SymbolKind::Function, "src/new.rs", 0, 20)
        .hashes(11, 22);
    let changes = classify_in(
        base.finish(),
        head.finish(),
        &[renamed_file("src/old.rs", "src/new.rs")],
    );
    assert_eq!(summary(&changes), ["body krate::f"]);
}

#[test]
fn orders_changes_by_file_offset_kind_and_path() {
    let mut base = GraphBuilder::new();
    base.add("krate::a::f", SymbolKind::Function, "src/a.rs", 10, 20)
        .hashes(11, 21);
    base.add("krate::a::gone", SymbolKind::Function, "src/a.rs", 50, 60)
        .hashes(12, 22);
    base.add("krate::b::h", SymbolKind::Function, "src/b.rs", 0, 10)
        .hashes(13, 23);
    let mut head = GraphBuilder::new();
    head.add("krate::a::f", SymbolKind::Function, "src/a.rs", 10, 20)
        .hashes(11, 25);
    head.add("krate::b::h", SymbolKind::Function, "src/b.rs", 0, 10)
        .hashes(14, 23);
    let changed = [modified("src/a.rs"), modified("src/b.rs")];
    let changes = classify_in(base.finish(), head.finish(), &changed);
    assert_eq!(
        summary(&changes),
        [
            "body krate::a::f",
            "removed krate::a::gone",
            "signature krate::b::h",
        ]
    );
}

#[test]
fn changes_at_one_location_follow_the_declaration_order_of_the_kinds() {
    let mut base = GraphBuilder::new();
    base.add("krate::v", SymbolKind::Function, "src/lib.rs", 10, 20)
        .hashes(11, 21)
        .vis(Visibility::Private);
    let mut head = GraphBuilder::new();
    head.add("krate::v", SymbolKind::Function, "src/lib.rs", 10, 20)
        .hashes(11, 22)
        .vis(Visibility::Public);
    let changes = classify_in(base.finish(), head.finish(), &[modified("src/lib.rs")]);
    assert_eq!(
        summary(&changes),
        ["body krate::v", "visibility krate::v Private->Public"]
    );
}

#[test]
fn classifying_twice_gives_identical_results() {
    let build_base = || {
        let mut base = GraphBuilder::new();
        base.add("krate::a::f", SymbolKind::Function, "src/a.rs", 0, 20)
            .hashes(11, 21);
        base.add("krate::a::gone", SymbolKind::Struct, "src/a.rs", 30, 60)
            .sig(12);
        let old = base
            .add("krate::b::Old", SymbolKind::Struct, "src/b.rs", 0, 40)
            .sig(13)
            .id();
        base.add("krate::b::Old::x", SymbolKind::Field, "src/b.rs", 20, 30)
            .sig(21)
            .parent(old);
        base.finish()
    };
    let build_head = || {
        let mut head = GraphBuilder::new();
        head.add("krate::a::f", SymbolKind::Function, "src/a.rs", 0, 24)
            .hashes(11, 22);
        let new = head
            .add("krate::b::New", SymbolKind::Struct, "src/b.rs", 0, 40)
            .sig(13)
            .id();
        head.add("krate::b::New::x", SymbolKind::Field, "src/b.rs", 20, 30)
            .sig(21)
            .parent(new);
        head.finish()
    };
    let changed = [modified("src/a.rs"), modified("src/b.rs")];
    let params = MatchParams::default();
    let first = classify(&build_base(), &build_head(), &changed, &params);
    let second = classify(&build_base(), &build_head(), &changed, &params);
    assert_eq!(first, second);
    assert_eq!(
        summary(&first),
        [
            "body krate::a::f",
            "removed krate::a::gone",
            "renamed krate::b::New from krate::b::Old",
        ]
    );
}

#[test]
fn the_default_similarity_is_point_seven() {
    assert!((MatchParams::default().rename_similarity - 0.7).abs() < f32::EPSILON);
}
