//! Constructing an `inner class` from inside the enclosing class (`Inner()`), which captures the
//! enclosing instance (`this$0`) so the inner body reads the outer's members. Same-file, runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn inner_class_construct_and_read_outer() {
    const SRC: &str = "class A(val x: Int) {\n\
        \x20 fun getx() = x + 1\n\
        \x20 inner class Inner {\n\
        \x20   fun r(): Int = x + getx()\n\
        \x20 }\n\
        \x20 fun make(): Inner = Inner()\n\
        }\n\
        fun box(): String = if (A(7).make().r() == 15) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("inner class construct"), "OK");
}

#[test]
fn inner_class_with_ctor_args() {
    const SRC: &str = "class A(val base: Int) {\n\
        \x20 inner class Inner(val add: Int) { fun r(): Int = base + add }\n\
        \x20 fun make(a: Int): Inner = Inner(a)\n\
        }\n\
        fun box(): String = if (A(10).make(5).r() == 15) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("inner class ctor args"), "OK");
}

const RECEIVER_INNER_POSTPONED: &str = "class W(val string: String)\n\
    class P {\n\
    \x20 inner class Q(val b: W)\n\
    \x20 inner class R(val f: () -> String)\n\
    }\n\
    fun f() = P().Q(W(\"O\"))\n\
    fun g() = P().R { \"K\" }\n\
    fun box(): String {\n\
    \x20 val k = g().f()\n\
    \x20 return f().b.string + k\n\
    }\n";

/// `outer.Inner(args)` whose arguments need an expected type (a constructor call, a lambda) binds
/// the receiver's inner class on its member level, so the caller's return type infers.
#[test]
fn a_receiver_inner_constructor_types_postponed_arguments() {
    common::assert_class_code_matches_kotlinc(
        "ReceiverInnerPostponed",
        RECEIVER_INNER_POSTPONED,
        "ReceiverInnerPostponedKt",
    );
    common::expect_box_same_as_kotlinc(RECEIVER_INNER_POSTPONED, "ReceiverInnerPostponedRun");
}

/// A receiver exposing both `fun Inner` and `inner class Inner` whose shapes are incomparable for
/// `(1, 2)`: explicit, safe, bare, and inherited calls are all ambiguous between the constructor and
/// the function, the constructor listed first and named by the class that declares it.
const MEMBER_LEVEL_AMBIGUITY: &str = "class Outer {\n\
    \x20   fun Inner(x: Int, y: Any): String = \"fun\"\n\
    \x20   inner class Inner(x: Any, y: Int)\n\
    \x20   fun implicit(): Any = Inner(1, 2)\n\
    }\n\
    open class Base {\n\
    \x20   fun Nested(x: Int, y: Any): String = \"fun\"\n\
    \x20   inner class Nested(x: Any, y: Int)\n\
    }\n\
    class Derived : Base() {\n\
    \x20   fun implicit(): Any = Nested(1, 2)\n\
    }\n\
    fun explicit(o: Outer): Any = o.Inner(1, 2)\n\
    fun safe(o: Outer?): Any? = o?.Inner(1, 2)\n\
    fun inherited(d: Derived): Any = d.Nested(1, 2)\n";

#[test]
fn inner_constructors_and_member_functions_are_one_overload_family() {
    common::assert_error_blocks_match_kotlinc(&[("Main.kt", MEMBER_LEVEL_AMBIGUITY)], &[]);
}

/// Inferred-return declarations select through the same member level: an argument type or a
/// lambda's arity decides between the inner constructor and the same-named function.
const MEMBER_LEVEL_SELECTION: &str = "class Outer(val tag: String) {\n\
    \x20   fun Inner(x: String) = \"fun\" + x\n\
    \x20   inner class Inner(val x: Int) { override fun toString() = tag + x }\n\
    \x20   fun implicit() = Inner(1)\n\
    \x20   fun implicitFun() = Inner(\"s\")\n\
    \x20   fun Make(f: (String) -> Int) = \"fun\" + f(\"ab\")\n\
    \x20   inner class Make(val f: (Int, Int) -> Int) { fun run() = f(2, 3) }\n\
    \x20   fun two() = Make { a, b -> a * b }\n\
    \x20   fun typed() = Make { s: String -> s.length }\n\
    }\n\
    fun explicit(o: Outer) = o.Inner(2)\n\
    fun explicitFun(o: Outer) = o.Inner(\"t\")\n\
    fun explicitLambda(o: Outer) = o.Make { a, b -> a + b }\n\
    fun safe(o: Outer?) = o?.Inner(3)\n\
    fun box(): String {\n\
    \x20   val o = Outer(\"O\")\n\
    \x20   if (o.implicit().toString() != \"O1\") return \"implicit\"\n\
    \x20   if (o.implicitFun() != \"funs\") return \"implicitFun\"\n\
    \x20   if (o.two().run() != 6) return \"two\"\n\
    \x20   if (o.typed() != \"fun2\") return \"typed\"\n\
    \x20   if (explicit(o).toString() != \"O2\") return \"explicit\"\n\
    \x20   if (explicitFun(o) != \"funt\") return \"explicitFun\"\n\
    \x20   if (explicitLambda(o).run() != 5) return \"explicitLambda\"\n\
    \x20   if (safe(o).toString() != \"O3\") return \"safe\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn member_level_selection_chooses_between_inner_constructors_and_functions() {
    common::expect_box_same_as_kotlinc(MEMBER_LEVEL_SELECTION, "MemberLevelSelection");
}
