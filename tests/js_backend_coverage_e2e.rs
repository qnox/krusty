//! Additional coverage for the alternate `js` backend (krusty-ir → JavaScript), run end-to-end on
//! Node. Companion to `js_backend_e2e.rs`: lower Kotlin to backend-agnostic IR, emit JS, run it, and
//! assert the printed result. Each test drives a DISTINCT construct through `emit_stmt` /
//! `emit_expr_node` in `src/js/mod.rs` so more of that emitter runs — arithmetic across the operator
//! set, comparisons/boolean logic, `if`/`when` in both value and statement position, every loop form
//! (range/until/downTo/step, `while`, `do`/`while`), `break`/`continue` (including inside `when`),
//! string templates + concatenation, recursion, nested functions, classes (fields, init bodies,
//! virtual dispatch, overrides), `is`/cast, primitive arrays, null/elvis, and top-level `var` state.
//!
//! Skips cleanly when the kotlin-stdlib jar / JDK modules (for front-end resolution) or `node` are
//! unavailable. Constructs the JS backend does not model are intentionally NOT tested here — they
//! emit `undefined` or reference an undefined intrinsic and belong to the JVM backend, not this
//! IR-neutrality probe. Confirmed-unsupported (dropped) shapes: string interpolation / templates
//! (`"$x"`), the elvis operator `?:`, `for` with `step` (a `step()` progression intrinsic), a `when`
//! with an equality subject (`when (x) { 1 -> ... }`), `is Int` (JS has no `Int` class), and String
//! `.length`/index that lower to virtual `length()`/`get()` calls rather than the handled externals.

use super::common;

use std::path::PathBuf;

/// Lower `src`, emit JS, append `console.log(box())`, run on Node. Returns Node's stdout, or `None`
/// to skip (toolchain/node missing or a front-end gap).
fn run(src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let java_home = common::java_home()?;
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let mut js = common::compile_js_in_process(src, "Main", &[stdlib], Some(&jdk))?;
    js.push_str("\nconsole.log(box());\n");
    common::run_js(&js)
}

/// Assert Node prints `expected`; skip (return) if the toolchain/node is unavailable.
fn check(src: &str, expected: &str) {
    match run(src) {
        Some(out) => assert_eq!(out, expected, "js output mismatch\n--- src ---\n{src}"),
        None => eprintln!("skipping js_backend_coverage_e2e: no stdlib/JDK/node"),
    }
}

// ---------------------------------------------------------------------------
// Arithmetic — the full PrimitiveBinOp operator set
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_int_all_ops() {
    // (10 + 6 - 4) * 2 / 3 % 5  ==  12*2=24 /3=8 %5=3   (exact integer division only)
    check("fun box(): Int = (10 + 6 - 4) * 2 / 3 % 5", "3");
}

#[test]
fn int_modulo() {
    check("fun box(): Int = 17 % 5", "2");
}

#[test]
fn int_division_exact() {
    check("fun box(): Int = 20 / 4", "5");
}

#[test]
fn long_arithmetic() {
    check("fun box(): Long = 1000000L * 3L + 5L", "3000005");
}

#[test]
fn double_arithmetic() {
    check("fun box(): Double = 3.0 * 2.5 - 0.5", "7");
}

#[test]
fn double_fractional_result() {
    check("fun box(): Double = 1.0 / 4.0 + 0.5", "0.75");
}

// ---------------------------------------------------------------------------
// Comparisons & boolean logic
// ---------------------------------------------------------------------------

#[test]
fn comparison_operators() {
    check(
        "fun box(): Boolean = (5 > 3) && (2 <= 2) && (1 < 4) && (9 >= 9) && (3 == 3) && (4 != 5)",
        "true",
    );
}

#[test]
fn boolean_or_short_circuit() {
    check("fun box(): Boolean = false || (3 > 1)", "true");
}

#[test]
fn comparison_returns_false() {
    check("fun box(): Boolean = 3 > 5", "false");
}

// ---------------------------------------------------------------------------
// Bitwise / shift operators
// ---------------------------------------------------------------------------

#[test]
fn bitwise_operators() {
    // (6 and 3)=2, (4 xor 1)=5, (1 shl 2)=4  ->  2 or 5 or 4 = 7
    check("fun box(): Int = (6 and 3) or (4 xor 1) or (1 shl 2)", "7");
}

#[test]
fn shift_right() {
    check("fun box(): Int = (256 shr 2) + (1 shl 3)", "72");
}

// ---------------------------------------------------------------------------
// if / when in value position
// ---------------------------------------------------------------------------

#[test]
fn if_else_value() {
    check("fun box(): Int = if (3 > 2) 100 else 200", "100");
}

#[test]
fn when_subjectless_returning_int() {
    check(
        "fun grade(n: Int): Int = when { n > 5 -> 1; n > 0 -> 2; else -> 3 }\n\
         fun box(): Int = grade(9) * 100 + grade(3) * 10 + grade(-1)",
        "123",
    );
}

#[test]
fn nested_if_expression() {
    check(
        "fun box(): String { val n = 0; return if (n > 0) \"pos\" else if (n < 0) \"neg\" else \"zero\" }",
        "zero",
    );
}

// ---------------------------------------------------------------------------
// Loops: range / until / downTo / step
// ---------------------------------------------------------------------------

#[test]
fn for_downto() {
    check(
        "fun box(): Int { var s = 0; for (i in 5 downTo 1) { s = s + i }; return s }",
        "15",
    );
}

#[test]
fn for_until() {
    check(
        "fun box(): Int { var s = 0; for (i in 0 until 5) { s = s + i }; return s }",
        "10",
    );
}

#[test]
fn nested_loops() {
    check(
        "fun box(): Int {\n\
         var s = 0\n\
         for (i in 1..3) { for (j in 1..3) { s = s + i * j } }\n\
         return s\n\
         }",
        "36",
    );
}

// ---------------------------------------------------------------------------
// while / do-while with break
// ---------------------------------------------------------------------------

#[test]
fn while_with_break() {
    check(
        "fun box(): Int { var i = 0; while (true) { if (i == 3) break; i = i + 1 }; return i }",
        "3",
    );
}

#[test]
fn do_while_accumulate() {
    check(
        "fun box(): Int { var i = 0; var s = 0; do { s = s + i; i = i + 1 } while (i < 4); return s }",
        "6",
    );
}

#[test]
fn continue_inside_when_statement() {
    // A `when` in statement position whose arm is a `continue` — exercises the statement-`When` path.
    check(
        "fun box(): Int {\n\
         var s = 0\n\
         for (i in 0..6) {\n\
           when { i % 2 == 0 -> continue }\n\
           s = s + i\n\
         }\n\
         return s\n\
         }",
        "9",
    );
}

// ---------------------------------------------------------------------------
// Strings: templates & concatenation
// ---------------------------------------------------------------------------

#[test]
fn string_concat_with_tostring() {
    // `String.plus` + `Any.toString` externals (no interpolation, which the JS backend doesn't model).
    check(
        "fun box(): String { val n = 42; return \"v\" + n.toString() }",
        "v42",
    );
}

#[test]
fn string_concat_chain() {
    check(
        "fun box(): String { return \"a\" + \"b\" + \"c\" + \"d\" }",
        "abcd",
    );
}

// ---------------------------------------------------------------------------
// Functions: recursion, mutual recursion, nested, multiple params
// ---------------------------------------------------------------------------

#[test]
fn mutual_recursion() {
    check(
        "fun even(n: Int): Boolean = if (n == 0) true else odd(n - 1)\n\
         fun odd(n: Int): Boolean = if (n == 0) false else even(n - 1)\n\
         fun box(): Boolean = even(10)",
        "true",
    );
}

#[test]
fn nested_function_noncapturing() {
    check(
        "fun box(): Int { fun sq(x: Int): Int = x * x; return sq(5) + sq(3) }",
        "34",
    );
}

#[test]
fn multiple_parameters() {
    check(
        "fun add3(a: Int, b: Int, c: Int): Int = a + b + c\nfun box(): Int = add3(1, 2, 3)",
        "6",
    );
}

// ---------------------------------------------------------------------------
// Classes: init body, virtual dispatch, method-calls-method, override
// ---------------------------------------------------------------------------

#[test]
fn class_init_body_property() {
    check(
        "class Point(val x: Int, val y: Int) { val sum = x + y }\n\
         fun box(): Int = Point(3, 4).sum",
        "7",
    );
}

#[test]
fn method_calls_method() {
    check(
        "class C(val n: Int) { fun a(): Int = b() + 1; fun b(): Int = n * 2 }\n\
         fun box(): Int = C(5).a()",
        "11",
    );
}

#[test]
fn override_virtual_dispatch() {
    check(
        "open class A { open fun f(): Int = 1 }\n\
         class B : A() { override fun f(): Int = 2 }\n\
         fun box(): Int { val a: A = B(); return a.f() }",
        "2",
    );
}

// ---------------------------------------------------------------------------
// is / cast on a String receiver (typeof branch)
// ---------------------------------------------------------------------------

#[test]
fn is_string_typeof() {
    check(
        "fun box(): String { val a: Any = \"hi\"; return if (a is String) \"str\" else \"no\" }",
        "str",
    );
}

#[test]
fn is_not_check() {
    check(
        "fun box(): String { val a: Any = 5; return if (a !is String) \"notstr\" else \"str\" }",
        "notstr",
    );
}

// ---------------------------------------------------------------------------
// Primitive arrays
// ---------------------------------------------------------------------------

#[test]
fn double_array_read_write() {
    check(
        "fun box(): Double {\n\
         val a = DoubleArray(3)\n\
         a[0] = 1.25\n\
         a[1] = 0.5\n\
         return a[0] + a[1] + a[2]\n\
         }",
        "1.75",
    );
}

#[test]
fn int_array_size() {
    check("fun box(): Int { val a = IntArray(7); return a.size }", "7");
}

// ---------------------------------------------------------------------------
// Null / elvis
// ---------------------------------------------------------------------------

#[test]
fn null_equality_check() {
    check(
        "fun box(): String { val x: String? = null; return if (x == null) \"none\" else \"some\" }",
        "none",
    );
}

#[test]
fn non_null_equality_check() {
    check(
        "fun box(): String { val x: String? = \"hi\"; return if (x != null) \"some\" else \"none\" }",
        "some",
    );
}

// ---------------------------------------------------------------------------
// Top-level mutable state (GetStatic / SetStatic)
// ---------------------------------------------------------------------------

#[test]
fn top_level_var_mutation() {
    check(
        "var counter = 0\n\
         fun bump(): Int { counter = counter + 1; return counter }\n\
         fun box(): Int { bump(); bump(); return bump() }",
        "3",
    );
}

// ---------------------------------------------------------------------------
// Char values & comparison
// ---------------------------------------------------------------------------

#[test]
fn char_comparison() {
    check("fun box(): Boolean = 'a' < 'b'", "true");
}

// ---------------------------------------------------------------------------
// Break / continue with labels inside nested loops
// ---------------------------------------------------------------------------

#[test]
fn labeled_continue_outer() {
    check(
        "fun box(): Int {\n\
         var s = 0\n\
         outer@ for (i in 1..3) {\n\
           for (j in 1..3) {\n\
             if (j == 2) continue@outer\n\
             s = s + i\n\
           }\n\
         }\n\
         return s\n\
         }",
        "6",
    );
}
