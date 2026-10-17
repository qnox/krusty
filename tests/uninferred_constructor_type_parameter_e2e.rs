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
