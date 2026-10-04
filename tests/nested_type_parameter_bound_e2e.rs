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
