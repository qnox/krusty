//! Overloaded `++`/`--` on a local variable whose type has a user `inc`/`dec` MEMBER operator —
//! desugared to `x = x.inc()` (statement / prefix / postfix; postfix yields the captured old value).
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_inc_local_all_forms() {
    const SRC: &str = "class N(val i: Int) { operator fun inc(): N = N(i + 1) }\n\
        fun box(): String {\n\
        \x20 var a = N(1)\n\
        \x20 a++\n\
        \x20 if (a.i != 2) return \"fail stmt: ${a.i}\"\n\
        \x20 val old = a++\n\
        \x20 if (old.i != 2 || a.i != 3) return \"fail postfix: ${old.i} ${a.i}\"\n\
        \x20 val nw = ++a\n\
        \x20 if (nw.i != 4 || a.i != 4) return \"fail prefix: ${nw.i} ${a.i}\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("member inc"), "OK");
}

#[test]
fn member_inc_on_field_and_index_statement() {
    // `obj.x++` / `arr[i]++` (statement position) desugar to `... = ....inc()`, so a user `inc`
    // operator works on a member/index target too.
    const SRC: &str = "class N(val i: Int) { operator fun inc(): N = N(i + 1) }\n\
        class Box(var ref: N)\n\
        fun box(): String {\n\
        \x20 val b = Box(N(5))\n\
        \x20 b.ref++\n\
        \x20 b.ref++\n\
        \x20 val a = arrayOf(N(1))\n\
        \x20 a[0]++\n\
        \x20 return if (b.ref.i == 7 && a[0].i == 2) \"OK\" else \"fail ${b.ref.i} ${a[0].i}\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("member/index inc"), "OK");
}

#[test]
fn extension_inc_on_nullable_user_class() {
    // A nullable-receiver operator EXTENSION on a MODULE-declared class (`operator fun C?.inc()`) is
    // safe (no builtin collision) and drives `x++` via a static extension call.
    const SRC: &str = "class C(val n: Int)\n\
        operator fun C?.inc(): C? = C((this?.n ?: 0) + 1)\n\
        fun box(): String {\n\
        \x20 var c: C? = C(5)\n\
        \x20 val old = c++\n\
        \x20 return if (old!!.n == 5 && c!!.n == 6) \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("extension inc"), "OK");
}

#[test]
fn member_dec_local() {
    const SRC: &str = "class N(val i: Int) { operator fun dec(): N = N(i - 1) }\n\
        fun box(): String {\n\
        \x20 var a = N(5)\n\
        \x20 a--\n\
        \x20 val old = a--\n\
        \x20 return if (a.i == 3 && old.i == 4) \"OK\" else \"fail ${a.i} ${old.i}\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("member dec"), "OK");
}

/// An inc/dec as a block's TRAILING value (`{ -> p.fst++ }` is `() -> Int`, not `() -> Unit`):
/// the parser keeps it as the block's trailing expression — a `Name` target lowers directly, a
/// member/index target desugars to a temp block that captures the old (postfix) or new (prefix)
/// value. Previously the statement re-route fired unconditionally and the lambda yielded `Unit`
/// (a `ClassCastException` downstream — inline/lambdaReassignmentWithCapture.kt).
#[test]
fn incdec_trailing_lambda_value_member_target() {
    const SRC: &str = "class P(var fst: Int, var snd: Int)\n\
        fun box(): String {\n\
        \x20 val p = P(0, 0)\n\
        \x20 val post: () -> Int = { -> p.fst++ }\n\
        \x20 if (post() != 0 || p.fst != 1) return \"fail postfix: ${p.fst}\"\n\
        \x20 val pre: () -> Int = { -> ++p.snd }\n\
        \x20 if (pre() != 1 || p.snd != 1) return \"fail prefix: ${p.snd}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("trailing member incdec"), "OK");
}

/// Same trailing-position rule for a local-variable target (no desugar needed — the expression
/// form lowers directly).
#[test]
fn incdec_trailing_lambda_value_local_target() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var x = 10\n\
        \x20 val post: () -> Int = { -> x++ }\n\
        \x20 if (post() != 10 || x != 11) return \"fail postfix: $x\"\n\
        \x20 val pre: () -> Int = { -> ++x }\n\
        \x20 if (pre() != 12 || x != 12) return \"fail prefix: $x\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("trailing local incdec"), "OK");
}

/// The `inline/lambdaReassignmentWithCapture.kt` shape: aliased, reassigning lambdas passed as
/// function-typed VARIABLE arguments to a cross-file inline facade static.
#[test]
fn trailing_incdec_lambda_reassignment_with_capture() {
    const LIB: &str = "package foo\n\
                       data class IntPair(public var fst: Int, public var snd: Int)\n\
                       inline fun run(func: () -> Int): Int {\n\
                       \x20   return func()\n\
                       }\n";
    const MAIN: &str = "package foo\n\
                        fun bar(p: IntPair): Int {\n\
                        \x20   var f = { -> p.fst++ }\n\
                        \x20   var get0 = f\n\
                        \x20   f = { -> ++p.snd }\n\
                        \x20   var get1 = f\n\
                        \x20   var get2 = get1\n\
                        \x20   f = { -> ++p.fst }\n\
                        \x20   get2 = f\n\
                        \x20   return run(get0) + run(get1) + run(get2)\n\
                        }\n\
                        fun box(): String {\n\
                        \x20   val p = IntPair(0, 0)\n\
                        \x20   if (bar(p) != 3) return \"fail\"\n\
                        \x20   return if (p.fst == 2 && p.snd == 1) \"OK\" else \"fail: $p\"\n\
                        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("lib.kt", LIB), ("main.kt", MAIN)],
        "inline_lambda_reassignment_capture",
    );
}

/// A non-lvalue inc/dec target in a lambda's trailing position is an honest parse error (never a
/// compiler panic and never a double evaluation).
#[test]
fn non_lvalue_trailing_incdec_is_a_parse_error() {
    let diags = common::front_end_diagnostics(
        "fun foo(): Int = 1\n\
         fun box(): String {\n\
         \x20 val f = { -> foo()++ }\n\
         \x20 return \"unreachable\"\n\
         }\n",
        &[],
        None,
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("inc/dec with a non-lvalue or side-effecting target")),
        "expected the non-lvalue inc/dec diagnostic, got: {diags:?}"
    );
}
