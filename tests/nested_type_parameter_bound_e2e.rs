//! A type-parameter bound resolves its classifier in the declaration's lexical scope. A bound
//! that names a nested classifier keeps it in the `Signature` attribute and the metadata,
//! instead of erasing to `Object` through the module-wide spelling table.

use super::common;

const ACCEPTED: &str = r#"
open class Own
class Outer {
    open class Own
    fun <T : Own> picksNested(t: T): T = t
    class Deeper {
        fun <T : Own> picksOuter(t: T): T = t
        open class Own
        fun <T : Own> picksDeeper(t: T): T = t
    }
    class Gen<T : Own>(val t: T)
    companion object {
        open class Tag
    }
    fun <T : Tag> companionNested(t: T): T = t
    val <T : Own> T.prop: T get() = this
    interface Api { fun <T : Own> api(t: T): T }
}
fun <T : Own> topLevel(t: T): T = t

fun box(): String {
    val outer = Outer()
    val nested = Outer.Own()
    if (outer.picksNested(nested) !== nested) return "nested"
    if (Outer.Gen(nested).t !== nested) return "gen"
    val tag = Outer.Companion.Tag()
    if (outer.companionNested(tag) !== tag) return "companion"
    val deeper = Outer.Deeper.Own()
    if (Outer.Deeper().picksDeeper(deeper) !== deeper) return "deeper"
    val top = Own()
    if (topLevel(top) !== top) return "top"
    return "OK"
}
"#;

#[test]
fn nested_classifier_bounds_are_signed_like_kotlinc() {
    common::assert_accepted_like_kotlinc(ACCEPTED);
    assert_eq!(
        common::expect_box_run_with_stdlib(ACCEPTED, "NestedBounds"),
        "OK"
    );
    common::assert_classes_identical_to_kotlinc(
        "NestedBounds",
        ACCEPTED,
        &[
            "Outer",
            "Outer$Deeper",
            "Outer$Gen",
            "Outer$Api",
            "NestedBoundsKt",
        ],
    );
}

const PACKAGE_A: &str = r#"package a

open class Item
interface Entity<S : Entity<S>>
class Box<T : Item>(val t: T)
fun <T : Item> pickA(t: T): T = t
"#;

const PACKAGE_B: &str = r#"package b

open class Item
interface Entity<S : Entity<S>>
class Box<T : Item>(val t: T)
fun <T : Item> pickB(t: T): T = t
"#;

const IMPORTING: &str = r#"package c

import b.Item
import a.Entity

class Holder<T : Item>(val t: T)
fun <T : Item> pickC(t: T): T = t
fun <S : Entity<S>> entity(s: S): S = s
fun <T : a.Item> qualified(t: T): T = t
class QualifiedBox<T : a.Box<a.Item>>(val t: T)
class Sorted<T : Comparable<T>>
abstract class Ordered<S : Ordered<S>> : Comparable<S>

class Outer {
    interface Node<N : Node<N>>
    fun <N : Node<N>> walk(n: N): N = n
}

fun box(): String {
    val item = Item()
    if (pickC(item) !== item) return "pick"
    if (Holder(item).t !== item) return "holder"
    val other = a.Item()
    if (qualified(other) !== other) return "qualified"
    return "OK"
}
"#;

/// Both packages declare `Item` and `Entity`, so the module-wide simple-name table gives neither
/// spelling one meaning. Each bound binds through its own file's package and imports, and a
/// classifier's own bound sees the classifier itself.
#[test]
fn bounds_bind_through_their_files_package_and_imports() {
    let sources = [
        ("ModelA.kt", PACKAGE_A),
        ("ModelB.kt", PACKAGE_B),
        ("Use.kt", IMPORTING),
    ];
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).expect("compile and run the module"),
        "OK"
    );
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

const AMBIGUOUS: &str = r#"package d

import a.*
import b.*

fun <T : Item> ambiguous(t: T): T = t
class AmbiguousBox<T : Item>
"#;

/// Two star imports make `Item` ambiguous; the bound reports that instead of picking one.
#[test]
fn an_ambiguous_imported_bound_is_rejected_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[
            ("ModelA.kt", PACKAGE_A),
            ("ModelB.kt", PACKAGE_B),
            ("Ambiguous.kt", AMBIGUOUS),
        ],
        &[],
    );
}

const SHADOWED_OWNER: &str = r#"package p

class A {
    class B
}
"#;

const SHADOWING: &str = r#"package u

import p.A

class Outer {
    class A
    fun <T : A.B> pick(t: T): T = t
    fun take(b: A.B): A.B = b
    class Holder<T : A.B>(val t: T)
}

fun box(): String {
    val b = p.A.B()
    if (Outer().pick(b) !== b) return "pick"
    if (Outer().take(b) !== b) return "take"
    if (Outer.Holder(b).t !== b) return "holder"
    return "OK"
}
"#;

/// A type reference binds among complete paths: the lexical `Outer.A` has no `B`, so `A.B` is
/// the imported `p.A.B`, which kotlinc accepts (warning only that the bound is final).
#[test]
fn a_qualified_bound_reaches_past_a_nearer_root_without_the_suffix() {
    let sources = [("Owner.kt", SHADOWED_OWNER), ("Shadowing.kt", SHADOWING)];
    let result = common::compiler_diagnostics(&sources, &[]);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    assert_eq!(common::compiler_errors(&result.krusty_stderr), []);
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).expect("compile and run the module"),
        "OK"
    );
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

const COMPLETE_STAR_ROOT: &str = r#"package complete

class ImportedRoot {
    open class Leaf
}
"#;

const INCOMPLETE_STAR_ROOT: &str = r#"package incomplete

class ImportedRoot
"#;

const COMPLETE_STAR_USE: &str = r#"package staruse

import complete.*
import incomplete.*

class Holder<T : ImportedRoot.Leaf>(val value: T)

fun box(): String {
    val leaf = ImportedRoot.Leaf()
    return if (Holder(leaf).value === leaf) "OK" else "wrong"
}
"#;

/// Star imports are one precedence rung, but only complete type paths participate in that rung's
/// ambiguity. An imported root without `Leaf` cannot make `ImportedRoot.Leaf` ambiguous.
#[test]
fn an_incomplete_star_import_root_does_not_hide_the_complete_path() {
    let sources = [
        ("Complete.kt", COMPLETE_STAR_ROOT),
        ("Incomplete.kt", INCOMPLETE_STAR_ROOT),
        ("Use.kt", COMPLETE_STAR_USE),
    ];
    let result = common::compiler_diagnostics(&sources, &[]);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    assert_eq!(common::compiler_errors(&result.krusty_stderr), []);
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).expect("compile and run the module"),
        "OK"
    );
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

const SECOND_COMPLETE_STAR_ROOT: &str = r#"package secondcomplete

class ImportedRoot {
    open class Leaf
}
"#;

const AMBIGUOUS_COMPLETE_STAR_USE: &str = r#"package staruse

import complete.*
import secondcomplete.*

class Holder<T : ImportedRoot.Leaf>
"#;

/// When both roots complete the path, the final classifier identities remain ambiguous.
#[test]
fn two_complete_star_import_paths_are_rejected_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[
            ("Complete.kt", COMPLETE_STAR_ROOT),
            ("SecondComplete.kt", SECOND_COMPLETE_STAR_ROOT),
            ("Use.kt", AMBIGUOUS_COMPLETE_STAR_USE),
        ],
        &[],
    );
}
