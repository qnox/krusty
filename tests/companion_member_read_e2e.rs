//! Unqualified `companion object` member reads (`result` instead of `Outer.result` /
//! `Companion.result`) from the outer class, nested classes, init blocks, lambdas, and the
//! companion's own members.
//!
//! Companion properties are static fields on the OUTER class (kotlinc's layout); the qualified
//! `Outer.X` read already emits `getstatic Outer.X`, but an unqualified read fell through every
//! `expr_inner_name` branch (locals → statics → members → outer-this) and bailed the file. The
//! lowering now walks the enclosing-class chain of the current class (`Outer$Nested$1` →
//! `Outer$Nested` → `Outer`, stripping a trailing `$Companion`) and reads the same static field —
//! AFTER real member lookups (a real member shadows a same-named companion member), and never for
//! a `private` companion property (its field is emitted private; a cross-class read would be an
//! IllegalAccessError without nestmate support).

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// From an outer-class method.
#[test]
fn companion_read_from_outer_method() {
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
    }
    fun test() = result
}
fun box() = Outer().test()
"#,
        "CompOuterMethod",
    );
}

/// From the companion's own method (and init-adjacent contexts).
#[test]
fn companion_read_from_companion_method() {
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
        fun get() = result
    }
}
fun box() = Outer.get()
"#,
        "CompOwnMethod",
    );
}

/// From a nested class (the corpus shape: `private companion object` with a public member).
#[test]
fn companion_read_from_nested_class() {
    run_box(
        r#"
class Outer {
    private companion object {
        val result = "OK"
    }

    class Nested {
        fun foo() = result
    }

    fun test() = Nested().foo()
}

fun box() = Outer().test()
"#,
        "CompNested",
    );
}

/// From an `init` block and a lambda inside a nested class.
#[test]
fn companion_read_from_init_and_lambda() {
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
    }

    val test: String

    init {
        test = result
    }
}

fun box() = Outer().test
"#,
        "CompInit",
    );
    run_box(
        r#"
class Outer {
    companion object {
        val result = "OK"
    }

    class Nested {
        fun foo(): String {
            val r = Runnable { result }
            return r.get()
        }
    }

    fun test() = Nested().foo()
}

interface Runnable { fun get(): String }

fun box() = Outer().test()
"#,
        "CompLambdaNested",
    );
}

/// BOUNDARY: a companion member that collides with an instance member is deliberately rejected
/// at the front end (a `Companion.` qualifier would be needed) — pinned so a future fix promotes it.
#[test]
fn member_companion_collision_stays_rejected() {
    let src = r#"
class Outer {
    val result = "member"

    companion object {
        val result = "companion"
    }

    fun test() = result
}

fun box() = Outer().test()
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "CompCollision").is_none(),
        "the member/companion collision gate must stay"
    );
}

/// From a receiver-lambda / scope function spliced into a member (`x.run { result }`).
#[test]
fn companion_read_in_scope_lambda() {
    run_box(
        r#"
class Outer {
    companion object { val result = "OK" }
    fun test(): String {
        val x = "x"
        return x.run { result }
    }
}
fun box() = Outer().test()
"#,
        "CompScopeLambda",
    );
}

/// BOUNDARY: in an inner class, an OUTER member currently beats the inner class's own companion
/// member on a name collision (kotlinc's receiver priority puts the companion first) — pre-existing
/// ordering; pinned so a future fix promotes it.
#[test]
fn inner_companion_vs_outer_member_boundary() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let src = r#"
class Outer {
    val x = "outer"
    inner class Nested {
        companion object { val x = "companion" }
        fun foo() = x
    }
    fun test() = Nested().foo()
}
fun box() = Outer().test()
"#;
    let out = common::compile_and_run_with_stdlib(src, "CompPrecedence");
    assert_ne!(
        out.as_deref(),
        Some("companion"),
        "the outer member must never silently lose to the inner companion"
    );
}

/// REJECTION GUARD: a `private` companion property read cross-class (its field is emitted
/// private; no nestmate support) must not compile to a direct getstatic.
#[test]
fn private_companion_prop_cross_class_still_rejected() {
    let src = r#"
class Outer {
    companion object {
        private val result = "OK"
    }

    class Nested {
        fun foo() = result
    }

    fun test() = Nested().foo()
}

fun box() = Outer().test()
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "CompPrivateCross").is_none(),
        "a private companion property read cross-class must not compile to a direct getstatic"
    );
}

/// The exact corpus cases. Companion METHOD calls from nested classes
/// (`privateCompanionObjectAccessedFromNestedClassSeveralTimes.kt`), the companion object AS A
/// VALUE (`privateCompanionObjectUsedInNestedClass.kt`), and the generic-HOF lambda case
/// (`privateCompanionObjectAccessedFromLambdaInNestedClass.kt`, whose `eval {}` return needs
/// generic substitution in signature inference) are separate gaps — pinned as boundaries below.
#[test]
fn corpus_companion_access_box_ok() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "objects/companionObjectAccess/privateCompanionObjectAccessedFromNestedClass.kt",
        "objects/companionObjectAccess/privateCompanionObjectAccessedFromInitBlock.kt",
        "objects/companionObjectAccess/privateCompanionObjectAccessedFromInitBlockOfNestedClass.kt",
        "objects/companionObjectAccess/privateCompanionObjectAccessedFromAnonymousObjectInNestedClass.kt",
        "objects/companionObjectAccess/protectedCompanionObjectAccessedFromNestedClass.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must execute successfully, not silently skip"
        );
    }
}

/// Companion METHOD calls, the companion object AS A VALUE, and the generic-HOF lambda case stay
/// skipped (separate features from the property read this file covers).
#[test]
fn boundary_companion_method_and_value_stay_skipped() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "objects/companionObjectAccess/privateCompanionObjectAccessedFromNestedClassSeveralTimes.kt",
        "objects/companionObjectAccess/privateCompanionObjectUsedInNestedClass.kt",
        "objects/companionObjectAccess/privateCompanionObjectAccessedFromLambdaInNestedClass.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case),
            None,
            "{case} needs machinery outside this feature — must stay skipped"
        );
    }
}
