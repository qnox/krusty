//! A constructor call whose class type parameter nothing constrains: no argument, written type
//! argument, expected type or enclosing call. kotlinc reports `cannot infer type for type
//! parameter` at the callee, as it does for a generic function result (`val x = make()`); krusty
//! completed the parameter to its declared bound and accepted the call. A self type
//! (`SELF : Builder<SELF>` in `Builder`) is the one such parameter Kotlin completes, and only when
//! its bound names no other formal: `MB("v")` for `MB<T, SELF : MB<T, SELF>>` is still reported.

use super::common;

const CLASSES: &str = "class C<T : Comparable<T>>(val n: Int)\n\
    class D<T>(val n: Int)\n\
    class P<A, B>(val a: A)\n\
    class KB<T : KB<T>>(val n: Int)\n\
    open class MB<T, SELF : MB<T, SELF>>(val v: T)\n\
    class Seq<T : CharSequence>(val x: T)\n\
    class Named<T : CharSequence>(val n: Int = 0, val x: T)\n";

#[test]
fn an_unconstrained_constructor_type_parameter_is_not_inferred() {
    let main = "fun use(c: C<String>) = c.n\n\
        class Tree<T>(val label: String) {\n\
            fun copy(): Tree<T> { val t = Tree<T>(label); return t }\n\
        }\n\
        fun box(): String {\n\
            val c = C(1)\n\
            val d = D(2)\n\
            val p = P(3)\n\
            val kb = KB(4)\n\
            val mb = MB(\"v\")\n\
            val typed: C<Int> = C(5)\n\
            val written = D<String>(6)\n\
            use(C(7))\n\
            val s: CharSequence = \"\"\n\
            val seq = Seq(s)\n\
            val named = Named(x = s)\n\
            when (s) { \"c\" -> D(8) }\n\
            return \"OK\"\n\
        }\n";
    let result = common::compiler_diagnostics(&[("Classes.kt", CLASSES), ("Main.kt", main)], &[]);
    common::expect_identical_rejection(&result, "uninferred_constructor_type_parameter");
}

#[test]
fn a_dependency_constructor_type_parameter_is_not_inferred_either() {
    let lib = format!("package lib\n{CLASSES}");
    let Some(classes) = common::compile_lib("uninferred_constructor_dependency", &lib) else {
        return;
    };
    let main = "import lib.*\n\
        fun box(): String {\n\
            val c = C(1)\n\
            val p = P(\"a\")\n\
            val kb = KB(2)\n\
            val typed: D<Int> = D(3)\n\
            val s: CharSequence = \"\"\n\
            val seq = Seq(s)\n\
            val named = Named(x = s)\n\
            return \"OK\"\n\
        }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", main)], &[classes]);
    common::expect_identical_rejection(&result, "uninferred_dependency_constructor");
}

#[test]
fn every_unconstrained_joined_branch_is_still_rejected() {
    let main = "class D<T>(val n: Int)\n\
        fun box(flag: Boolean): String {\n\
            if (flag) D(1) else D(2)\n\
            when (flag) { true -> D(3); else -> D(4) }\n\
            return \"OK\"\n\
        }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", main)], &[]);
    common::expect_identical_rejection(&result, "all unconstrained joined constructors");
}

#[test]
fn a_rejected_dependency_constructor_does_not_publish_inference_evidence() {
    let Some(classes) = common::compile_lib(
        "uninferred_private_constructor_dependency",
        "package lib\nclass Hidden<T> private constructor()\n",
    ) else {
        return;
    };
    let main = "import lib.Hidden\nfun box(): String { Hidden(); return \"OK\" }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", main)], &[classes]);
    common::expect_identical_rejection(&result, "rejected dependency constructor evidence");
}

/// A branch of a `when` with an `else` arm joins its siblings' inference even as a statement: the
/// first arm fixes `TV`, so the bare `Buildee()` arm is accepted. An inner class of a generic class
/// has no type parameter of its own to infer.
#[test]
fn a_joined_branch_and_an_inner_class_constructor_are_accepted() {
    common::expect_box_same_as_kotlinc(
        r#"
class TargetType
class Buildee<TV> {
    fun setTypeVariable(value: TV) { storage = value }
    private var storage: TV = TargetType() as TV
}
fun <PTV> build(instructions: Buildee<PTV>.() -> Unit): Buildee<PTV> = Buildee<PTV>().apply(instructions)

class Outer<T>(val x: T) {
    inner class Inner(val y: Int) { val xx = x }
}

fun box(): String {
    if (true) build { setTypeVariable(TargetType()) } else Buildee()
    when ("") {
        "true" -> build { setTypeVariable(TargetType()) }
        "false" -> build {}
        else -> Buildee()
    }
    val inner = Outer(42).Inner(100)
    return if (inner.xx == 42 && inner.y == 100) "OK" else "FAIL"
}
"#,
        "JoinedBranchConstructor",
    );
}
