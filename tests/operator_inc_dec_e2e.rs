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
fn trailing_implicit_property_inc_in_unit_extension_stays_a_statement() {
    // A function block discards its trailing expression. Keep this implicit-receiver property on
    // the statement path; only value-consuming lambda/if/when/try blocks may preserve `IncDec` as
    // an expression. This is the boundary that a global "before `}` means value" rule violated.
    const SRC: &str = "class SyntheticMutable(var value: Int)\n\
        fun SyntheticMutable.bump() { value++ }\n\
        fun box(): String {\n\
        \x20 val mutable = SyntheticMutable(1)\n\
        \x20 mutable.bump()\n\
        \x20 return if (mutable.value == 2) \"OK\" else \"fail: ${mutable.value}\"\n\
        }\n";
    assert_eq!(run(SRC).expect("implicit property inc statement"), "OK");
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

#[test]
fn smart_casted_nullable_primitive_incdec_uses_the_proven_read_type() {
    const SRC: &str = r#"
fun box(): String {
    var value: Int?
    value = 10

    val old: Int = value++
    if (old != 10 || value != 11) return "fail postfix: $old/$value"

    val updated: Int = ++value
    if (updated != 12 || value != 12) return "fail prefix: $updated/$value"

    return "OK"
}
"#;
    let (code, stderr) = common::kotlinc_source_result("SmartCastedNullablePrimitiveIncDec", SRC);
    assert_eq!(code, 0, "kotlinc rejected smart-cast inc/dec: {stderr}");
    assert_eq!(
        run(SRC).expect("smart-cast nullable primitive inc/dec"),
        "OK"
    );
}

#[test]
fn smart_casted_postfix_inference_uses_the_proven_read_type() {
    const SRC: &str = r#"
fun box(): String {
    var value: Int?
    value = 10

    var old = value++
    old = null
    return "unreachable: $old/$value"
}
"#;
    let (code, stderr) = common::kotlinc_source_result("SmartCastedPostfixInference", SRC);
    assert_ne!(code, 0, "kotlinc unexpectedly accepted null assignment");
    assert!(
        stderr.contains("null cannot be a value of a non-null type 'Int'"),
        "unexpected kotlinc diagnostic: {stderr}"
    );
    let diags = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diags
            .iter()
            .any(|diag| diag.contains("null cannot be a value of a non-null type 'Int'")),
        "expected inferred Int diagnostic, got: {diags:?}"
    );
}

#[test]
fn smart_casted_reference_incdec_selects_the_subtype_operator() {
    const SRC: &str = r#"
open class Base(val value: Int)
class Derived(value: Int) : Base(value)

operator fun Derived.inc(): Derived = Derived(value + 1)

fun box(): String {
    var current: Base
    current = Derived(20)

    val old: Derived = current++
    if (old.value != 20 || current.value != 21) return "fail postfix"

    val updated: Derived = ++current
    if (updated.value != 22 || current.value != 22) return "fail prefix"

    return "OK"
}
"#;
    let (code, stderr) = common::kotlinc_source_result("SmartCastedReferenceIncDec", SRC);
    assert_eq!(code, 0, "kotlinc rejected smart-cast inc/dec: {stderr}");
    assert_eq!(run(SRC).expect("smart-cast reference inc/dec"), "OK");
}

#[test]
fn smart_casted_incdec_statements_use_the_proven_read_type() {
    const SRC: &str = r#"
open class Base(val value: Int)
class Derived(value: Int) : Base(value)

operator fun Derived.inc(): Derived = Derived(value + 1)

fun box(): String {
    var number: Int?
    number = 4
    number++
    ++number

    var current: Base
    current = Derived(30)
    current++
    ++current

    return if (number == 6 && current.value == 32) "OK" else "fail: $number/${current.value}"
}
"#;
    let (code, stderr) = common::kotlinc_source_result("SmartCastedIncDecStatements", SRC);
    assert_eq!(code, 0, "kotlinc rejected smart-cast statements: {stderr}");
    assert_eq!(run(SRC).expect("smart-cast inc/dec statements"), "OK");
}

#[test]
fn prefix_incdec_expression_keeps_the_selected_receiver_type() {
    const SRC: &str = r#"
open class Base(val value: Int)
class Derived(value: Int) : Base(value)

operator fun Base.inc(): Derived = Derived(value + 1)

var top: Base = Base(10)

class Holder(var current: Base) {
    fun update(): Derived = ++current
}

fun box(): String {
    val topResult: Derived = ++top
    val holder = Holder(Base(20))
    val memberResult: Derived = holder.update()
    return "unreachable: ${topResult.value}/${memberResult.value}/${holder.current.value}"
}
"#;
    let (code, stderr) = common::kotlinc_source_result("PrefixIncDecSubtypeResult", SRC);
    assert_ne!(
        code, 0,
        "kotlinc unexpectedly exposed the inc return subtype"
    );
    assert!(
        stderr.contains("expected 'Derived', actual 'Base'"),
        "unexpected kotlinc diagnostic: {stderr}"
    );
    let diags = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diags
            .iter()
            .any(|diag| diag.contains("expected 'Derived', actual 'Base'")),
        "expected receiver-typed prefix diagnostic, got: {diags:?}"
    );
}

#[test]
fn local_prefix_incdec_result_type_matches_kotlinc_after_writeback() {
    const SRC: &str = r#"
open class Base(val value: Int)
class Derived(value: Int) : Base(value)

operator fun Base.inc(): Derived = Derived(value + 1)

fun update(): Derived {
    var current: Base = Base(30)
    return ++current
}

fun box(): String = if (update().value == 31) "OK" else "fail"
"#;
    let (code, stderr) = common::kotlinc_source_result("LocalPrefixIncDecResult", SRC);
    assert_eq!(code, 0, "kotlinc rejected local prefix result: {stderr}");
    assert_eq!(run(SRC).expect("local prefix inc/dec result"), "OK");
}

/// An inc/dec as a lambda block's TRAILING value (`{ -> p.fst++ }` is `() -> Int`, not `() -> Unit`):
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

/// Closing-block detection is generic rather than lambda-specific: an `if` branch exposes its last
/// expression as the branch value too. Prefix returns the assigned value and postfix returns the old
/// value; index targets exercise the same shared member/index assignment builder. The parser unit
/// regression separately inspects the expansion to prove a custom member getter is read only once.
#[test]
fn incdec_trailing_branch_value_and_index_target() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val values = intArrayOf(1, 4)\n\
        \x20 val prefix = if (true) { ++values[0] } else { -1 }\n\
        \x20 if (prefix != 2 || values[0] != 2) return \"fail prefix: $prefix/${values[0]}\"\n\
        \x20 val postfix = if (true) { values[0]++ } else { -1 }\n\
        \x20 if (postfix != 2 || values[0] != 3) return \"fail postfix: $postfix/${values[0]}\"\n\
        \x20 val indexed: () -> Int = { values[1]++ }\n\
        \x20 if (indexed() != 4 || values[1] != 5) return \"fail index: ${values[1]}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC).expect("trailing incdec access values"), "OK");
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

/// A non-lvalue inc/dec target in a block's trailing position is an honest parse error (never a
/// compiler panic and never a double evaluation). It shares the diagnostic with discarded-value
/// statement handling because both contexts use the same accepted-lvalue classifier.
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
            .any(|d| d.contains("only supported on a simple variable or pure access path")),
        "expected the non-lvalue inc/dec diagnostic, got: {diags:?}"
    );
}
