//! Coverage-focused end-to-end tests for the IR backend (`src/ir_lower.rs`, `src/jvm/ir_emit.rs`).
//!
//! Two kinds of test live here, both driving the FULL pipeline:
//!
//!  * `box_ok(src)` — compile `src` in-process (the exact `lex → parse → check → ir_lower →
//!    value-class → suspend → ir_emit` pipeline the conformance harness uses) and run its `box()` on a
//!    persistent JVM, asserting it returns `"OK"`. Each exercises an UNCOMMON-but-supported codegen
//!    branch (a rare numeric type/operator combination, a primitive array kind, a control-flow shape in
//!    an unusual position, an operator overload, a smart-cast/elvis/safe-call chain, …) that the box
//!    corpus does not otherwise reach.
//!
//!  * `rejects(src)` — compile `src` with the `krusty` binary and assert it declines CLEANLY (a
//!    non-zero exit or a "not (yet) supported" diagnostic). These hit the backend's `return None`
//!    bail branches ("this construct is not yet supported by the IR backend") for constructs krusty
//!    does not model yet — the backend declines rather than miscompiles. If one starts compiling, the
//!    feature has landed and the test should be promoted to a real round-trip.
//!
//! Both helpers SKIP CLEAN (pass) when the vendored kotlinc/JDK toolchain is unavailable, so the suite
//! never fails spuriously on a machine without it.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------------------------------
// box_ok: compile + run a supported snippet, expect "OK".
// ---------------------------------------------------------------------------------------------------

/// The `<stdlib jar>` + JDK `lib/modules` compile classpath, or `None` when the toolchain is absent.
fn box_cp() -> Option<(Vec<PathBuf>, Option<PathBuf>)> {
    let jh = common::java_home()?;
    let stdlib = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    if !jdk.exists() {
        return None;
    }
    Some((vec![stdlib], Some(jdk)))
}

/// Compile `src` and run its `box()`; assert `"OK"`. Skips clean when the toolchain is unavailable.
fn box_ok(src: &str) {
    let Some((cp, jdk)) = box_cp() else {
        return;
    };
    match common::compile_and_run_box(src, "Main", &cp, jdk.as_deref()) {
        Some(out) => assert_eq!(out, "OK", "box() returned {out:?} for src:\n{src}"),
        None => panic!("krusty failed to compile+run a supported construct:\n{src}"),
    }
}

// ---------------------------------------------------------------------------------------------------
// rejects: compile with the krusty binary, expect a clean decline.
// ---------------------------------------------------------------------------------------------------

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// `<stdlib>:<jdk modules>` classpath string, or `None` when the toolchain is unavailable.
fn reject_cp() -> Option<String> {
    let jh = common::java_home()?;
    let stdlib = common::stdlib_jar()?;
    let modules = format!("{jh}/lib/modules");
    if !std::path::Path::new(&modules).exists() {
        return None;
    }
    Some(format!("{}:{modules}", stdlib.to_string_lossy()))
}

/// Compile `src` with the `krusty` binary into a fresh temp dir. Returns `true` when the compile is
/// REJECTED — a non-zero exit or a "not (yet) supported" diagnostic — and `true` (skip-clean) when the
/// toolchain is absent.
fn rejects(src: &str) -> bool {
    let Some(cp) = reject_cp() else {
        return true;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("krusty_bailcov_{}_{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let kt = dir.join("S.kt");
    fs::write(&kt, src).unwrap();
    let out = Command::new(krusty)
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(&kt)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    !out.status.success()
        || stderr.contains("not yet supported")
        || stderr.contains("not supported")
        || stdout.contains("not yet supported")
}

// ===================================================================================================
// BOX-RUN TESTS — uncommon-but-supported codegen branches.
// ===================================================================================================

// --- numeric operators across the primitive types ---

#[test]
fn float_modulo() {
    box_ok("fun box(): String { val a = 5.5f % 2.0f; return if (a == 1.5f) \"OK\" else \"F\" }\n");
}

#[test]
fn bitwise_shift_and_logic_ops() {
    box_ok(
        "fun box(): String { val a = (1 shl 4) or (255 shr 2) xor 3 and 127; return if (a > 0) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn int_div_and_rem() {
    box_ok("fun box(): String { return if (7 / 2 == 3 && 7 % 2 == 1) \"OK\" else \"F\" }\n");
}

#[test]
fn int_bitwise_inv() {
    box_ok("fun box(): String { return if (0.inv() == -1) \"OK\" else \"F\" }\n");
}

#[test]
fn int_unsigned_shr() {
    box_ok("fun box(): String { return if ((-1) ushr 28 == 15) \"OK\" else \"F\" }\n");
}

#[test]
fn long_shift_left() {
    box_ok("fun box(): String { return if (1L shl 40 == 1099511627776L) \"OK\" else \"F\" }\n");
}

#[test]
fn long_multiply_wide() {
    box_ok(
        "fun box(): String { return if (1000000L * 1000000L == 1000000000000L) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn double_negate_and_compare() {
    box_ok("fun box(): String { val a = 1.5; return if (-a == -1.5) \"OK\" else \"F\" }\n");
}

#[test]
fn float_relational_ops() {
    box_ok(
        "fun box(): String { val a = 1.5f; val b = 2.5f; return if (a < b && b > a && a <= 1.5f) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn float_nan_and_infinity() {
    box_ok(
        "fun box(): String { val n = 0.0f / 0.0f; val i = 1.0f / 0.0f; return if (n.isNaN() && i.isInfinite()) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn int_compareto_method() {
    box_ok(
        "fun box(): String { return if (5.compareTo(3) > 0 && \"a\".compareTo(\"b\") < 0) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn compound_inc_dec_assign() {
    box_ok(
        "fun box(): String { var x = 5; x++; --x; x += 3; return if (x == 8) \"OK\" else \"F\" }\n",
    );
}

// --- Char arithmetic ---

#[test]
fn char_plus_int() {
    box_ok("fun box(): String { val c = 'A'; return if (c + 1 == 'B') \"OK\" else \"F\" }\n");
}

#[test]
fn char_minus_char() {
    box_ok("fun box(): String { return if ('z' - 'a' == 25) \"OK\" else \"F\" }\n");
}

#[test]
fn char_predicates_and_case() {
    box_ok(
        "fun box(): String { val c = '5'; return if (c.isDigit() && c.digitToInt() == 5 && 'a'.uppercaseChar() == 'A') \"OK\" else \"F\" }\n",
    );
}

// --- primitive arrays of every element kind ---

#[test]
fn boolean_array_ops() {
    box_ok(
        "fun box(): String { val a = BooleanArray(2); a[0] = true; return if (a[0] && !a[1]) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn float_array_ops() {
    box_ok(
        "fun box(): String { val a = FloatArray(2); a[0] = 1.5f; return if (a[0] == 1.5f) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn long_array_ops() {
    box_ok(
        "fun box(): String { val a = LongArray(2); a[0] = 5L; return if (a[0] == 5L) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn double_array_ops() {
    box_ok(
        "fun box(): String { val a = DoubleArray(2); a[0] = 1.5; return if (a[0] == 1.5) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn byte_array_compare() {
    box_ok(
        "fun box(): String { val a = ByteArray(2); a[0] = 5; a[1] = 3; return if (a[0] > a[1]) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn char_array_init_lambda() {
    box_ok(
        "fun box(): String { val a = CharArray(3) { 'a' + it }; return if (a[1] == 'b') \"OK\" else \"F\" }\n",
    );
}

// --- numeric conversions in every direction ---

#[test]
fn int_widening_conversions() {
    box_ok(
        "fun box(): String { val i = 65; return if (i.toChar() == 'A' && i.toByte().toInt() == 65 && i.toLong() == 65L && i.toDouble() == 65.0 && i.toFloat() == 65.0f) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn double_truncation_conversions() {
    box_ok(
        "fun box(): String { val d = 3.99; return if (d.toInt() == 3 && d.toLong() == 3L && (-3.99).toInt() == -3) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn long_narrowing_conversions() {
    box_ok(
        "fun box(): String { val l = 300L; return if (l.toByte().toInt() == 44 && l.toShort().toInt() == 300) \"OK\" else \"F\" }\n",
    );
}

// --- when / if / try in various positions ---

#[test]
fn when_no_subject() {
    box_ok(
        "fun box(): String { val x = 3; val r = when { x < 0 -> \"n\"; x == 0 -> \"z\"; else -> \"OK\" }; return r }\n",
    );
}

#[test]
fn when_subject_expression() {
    box_ok(
        "fun box(): String { val x = 2; return when (x) { 1 -> \"a\"; 2 -> \"OK\"; else -> \"b\" } }\n",
    );
}

#[test]
fn when_multi_value_label() {
    box_ok(
        "fun box(): String { val x = 2; return when (x) { 1, 2, 3 -> \"OK\"; else -> \"F\" } }\n",
    );
}

#[test]
fn when_range_arm() {
    box_ok(
        "fun box(): String { val x = 5; return when (x) { in 1..10 -> \"OK\"; else -> \"F\" } }\n",
    );
}

#[test]
fn when_is_type_arm() {
    box_ok(
        "fun box(): String { val x: Any = \"s\"; return when (x) { is Int -> \"i\"; is String -> \"OK\"; else -> \"o\" } }\n",
    );
}

#[test]
fn try_as_expression() {
    box_ok(
        "fun box(): String { val x = try { \"OK\" } catch (e: Exception) { \"F\" }; return x }\n",
    );
}

#[test]
fn return_inside_try_finally() {
    box_ok(
        "fun f(): Int { try { return 1 } finally { } }\nfun box(): String { return if (f() == 1) \"OK\" else \"F\" }\n",
    );
}

// --- labeled jumps and loops ---

#[test]
fn labeled_break() {
    box_ok(
        "fun box(): String { loop@ for (i in 0..9) { for (j in 0..9) { if (j == 2) break@loop } }; return \"OK\" }\n",
    );
}

#[test]
fn labeled_continue() {
    box_ok(
        "fun box(): String { var c = 0; outer@ for (i in 0..2) { for (j in 0..2) { if (j == 1) continue@outer; c++ } }; return if (c == 3) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn while_true_break() {
    box_ok(
        "fun box(): String { var i = 0; while (true) { if (i >= 5) break; i++ }; return if (i == 5) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn for_range_sum() {
    box_ok("fun box(): String { var s = 0; for (i in 1..5) s += i; return if (s == 15) \"OK\" else \"F\" }\n");
}

#[test]
fn for_downto_step() {
    box_ok(
        "fun box(): String { var s = 0; for (i in 5 downTo 1 step 2) s += i; return if (s == 9) \"OK\" else \"F\" }\n",
    );
}

// --- string operations ---

#[test]
fn string_builder_chain() {
    box_ok(
        "fun box(): String { val sb = StringBuilder(); sb.append(\"O\").append(\"K\").append(1); return if (sb.toString() == \"OK1\") \"OK\" else \"F\" }\n",
    );
}

#[test]
fn string_methods() {
    box_ok(
        "fun box(): String { val s = \"hello world\"; return if (s.substring(0, 5) == \"hello\" && s.split(\" \").size == 2 && s.replace(\"o\", \"0\") == \"hell0 w0rld\" && s.indexOf(\"world\") == 6) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn string_template_interpolation() {
    box_ok(
        "fun box(): String { val n = 5; val s = \"v=$n\"; return if (s == \"v=5\") \"OK\" else \"F\" }\n",
    );
}

// --- collections and nested generics ---

#[test]
fn list_of_and_size() {
    box_ok("fun box(): String { val l = listOf(1, 2, 3); return if (l.size == 3) \"OK\" else \"F\" }\n");
}

#[test]
fn nested_generic_map_of_list() {
    box_ok(
        "fun box(): String { val m = mapOf(\"a\" to listOf(1, 2), \"b\" to listOf(3)); return if (m[\"a\"]!!.size == 2) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn map_entry_destructure_loop() {
    box_ok(
        "fun box(): String { val m = mapOf(1 to \"a\", 2 to \"b\"); var s = 0; for ((k, v) in m) s += k; return if (s == 3) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn range_contains() {
    box_ok("fun box(): String { return if (5 in 1..10 && 15 !in 1..10) \"OK\" else \"F\" }\n");
}

// --- null-safety chains ---

#[test]
fn elvis_let_chain() {
    box_ok(
        "fun box(): String { val a: String? = null; val b: String? = \"x\"; return (a ?: b)?.let { \"OK\" } ?: \"F\" }\n",
    );
}

#[test]
fn safe_call_property_chain() {
    box_ok(
        "class A(val b: B?)\nclass B(val c: Int)\nfun box(): String { val a = A(B(5)); return if (a.b?.c == 5) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn not_null_assert() {
    box_ok("fun box(): String { val s: String? = \"OK\"; return s!! }\n");
}

#[test]
fn elvis_throw() {
    box_ok("fun box(): String { val s: String? = \"OK\"; return s ?: throw RuntimeException() }\n");
}

#[test]
fn elvis_self_assign() {
    box_ok("fun box(): String { var x: String? = null; x = x ?: \"OK\"; return x }\n");
}

#[test]
fn smart_cast_return() {
    box_ok(
        "fun f(x: Any): String { if (x is String) return x; return \"F\" }\nfun box(): String { return if (f(\"OK\") == \"OK\") \"OK\" else \"F\" }\n",
    );
}

// --- operator overloading and comparable ---

#[test]
fn operator_plus_overload() {
    box_ok(
        "class V(val n: Int) { operator fun plus(o: V) = V(n + o.n) }\nfun box(): String { return if ((V(2) + V(3)).n == 5) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn comparable_relational_operator() {
    box_ok(
        "class V(val n: Int) : Comparable<V> { override fun compareTo(o: V) = n - o.n }\nfun box(): String { return if (V(1) < V(2)) \"OK\" else \"F\" }\n",
    );
}

// --- class shapes: companion, data, enum, sealed, secondary ctor, init, generic ---

#[test]
fn companion_factory() {
    box_ok(
        "class C { companion object { fun make() = C() } }\nfun box(): String { C.make(); return \"OK\" }\n",
    );
}

#[test]
fn data_class_copy_and_destructure() {
    box_ok(
        "data class D(val a: Int, val b: String)\nfun box(): String { val d = D(1, \"x\"); val (a, b) = d; return if (a == 1 && b == \"x\" && d.copy(a = 2).a == 2) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn enum_ordinal() {
    box_ok(
        "enum class E { A, B, C }\nfun box(): String { return if (E.B.ordinal == 1) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn sealed_when_exhaustive() {
    box_ok(
        "sealed class S\nclass A : S()\nclass B : S()\nfun f(s: S) = when (s) { is A -> \"a\"; is B -> \"OK\" }\nfun box(): String { return f(B()) }\n",
    );
}

#[test]
fn secondary_constructor() {
    box_ok(
        "class C(val x: Int) { constructor() : this(7) }\nfun box(): String { return if (C().x == 7) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn init_block() {
    box_ok(
        "class C { val x: Int; init { x = 42 } }\nfun box(): String { return if (C().x == 42) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn generic_class_member() {
    box_ok(
        "class Box<T>(val v: T) { fun get() = v }\nfun box(): String { return if (Box(5).get() == 5 && Box(\"x\").get() == \"x\") \"OK\" else \"F\" }\n",
    );
}

// --- functions: extension, infix, generic, tailrec, local, anon object ---

#[test]
fn extension_function() {
    box_ok("fun Int.dbl() = this * 2\nfun box(): String { return if (3.dbl() == 6) \"OK\" else \"F\" }\n");
}

#[test]
fn infix_function() {
    box_ok(
        "infix fun Int.pw(n: Int): Int { var r = 1; repeat(n) { r *= this }; return r }\nfun box(): String { return if ((2 pw 3) == 8) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn generic_identity_function() {
    box_ok(
        "fun <T> id(x: T): T = x\nfun box(): String { return if (id(5) == 5 && id(\"OK\") == \"OK\") \"OK\" else \"F\" }\n",
    );
}

#[test]
fn tailrec_function() {
    box_ok(
        "tailrec fun f(n: Int, acc: Int): Int = if (n == 0) acc else f(n - 1, acc + n)\nfun box(): String { return if (f(5, 0) == 15) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn local_function() {
    box_ok(
        "fun box(): String { fun g(x: Int) = x * 2; return if (g(3) == 6) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn anonymous_object_interface() {
    box_ok(
        "interface I { fun f(): Int }\nfun box(): String { val o = object : I { override fun f() = 5 }; return if (o.f() == 5) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn anonymous_object_local_property() {
    box_ok("fun box(): String { val o = object { val x = 5 }; return if (o.x == 5) \"OK\" else \"F\" }\n");
}

#[test]
fn runnable_sam_lambda() {
    box_ok(
        "fun box(): String { var c = 0; val r = Runnable { c = 5 }; r.run(); return if (c == 5) \"OK\" else \"F\" }\n",
    );
}

// --- lambdas / inline / closures ---

#[test]
fn inline_lambda_call() {
    box_ok("inline fun run2(f: () -> Int) = f()\nfun box(): String { return if (run2 { 5 } == 5) \"OK\" else \"F\" }\n");
}

#[test]
fn lambda_labeled_return() {
    box_ok(
        "fun box(): String { val f = { x: Int -> if (x > 0) return@f \"OK\"; \"F\" }; return f(5) }\n",
    );
}

#[test]
fn nested_lambda_call() {
    box_ok("fun box(): String { val f = { { 5 } }; return if (f()() == 5) \"OK\" else \"F\" }\n");
}

#[test]
fn crossinline_lambda_capture() {
    box_ok(
        "inline fun run3(crossinline f: () -> Int): () -> Int = { f() }\nfun box(): String { return if (run3 { 5 }() == 5) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn noinline_lambda_return() {
    box_ok(
        "inline fun run4(noinline f: () -> Int) = f\nfun box(): String { return if (run4 { 5 }() == 5) \"OK\" else \"F\" }\n",
    );
}

// --- value / inline class ---

#[test]
fn jvm_inline_value_class() {
    box_ok(
        "@JvmInline value class W(val n: Int)\nfun box(): String { return if (W(5).n == 5) \"OK\" else \"F\" }\n",
    );
}

// ===================================================================================================
// REJECTS TESTS — the backend declines cleanly for constructs it does not model yet.
// ===================================================================================================

// --- delegated properties: distinct providers all deep-bail in ir_lower ---

#[test]
fn delegated_property_notnull_rejected() {
    assert!(rejects(
        "import kotlin.properties.Delegates\n\
         class C { var x: Int by Delegates.notNull() }\n\
         fun main() { val c = C(); c.x = 5; println(c.x) }\n"
    ));
}

#[test]
fn delegated_property_custom_class_rejected() {
    assert!(rejects(
        "class D\nclass C { var x: Int by D() }\nfun main() { println(C().x) }\n"
    ));
}

#[test]
fn local_delegated_val_rejected() {
    assert!(rejects(
        "fun f() { val x by lazy { 5 }; println(x) }\nfun main() { f() }\n"
    ));
}

// --- array-of-array constructors (`Array(n) { <primitive-array> }`) — explicit ir_lower bail ---

#[test]
fn array_of_double_array_ctor_rejected() {
    assert!(rejects(
        "fun main() { val a = Array(2) { DoubleArray(2) }; println(a.size) }\n"
    ));
}

// --- mixed spread with a leading fixed argument (SpreadBuilder path not modeled) — ir_lower bail ---

#[test]
fn leading_fixed_then_string_spread_rejected() {
    assert!(rejects(
        "fun f(vararg xs: String) = xs.size\n\
         fun main() { val a = arrayOf(\"a\", \"b\"); println(f(\"x\", *a)) }\n"
    ));
}

// --- constructs rejected earlier (checker/parser) but still a clean decline; the front end guards
//     the backend from ever seeing them. ---

#[test]
fn reified_type_param_usage_rejected() {
    assert!(rejects(
        "inline fun <reified T> name() = T::class.java.simpleName\n\
         fun main() { println(name<String>()) }\n"
    ));
}

#[test]
fn context_receiver_declaration_rejected() {
    assert!(rejects(
        "class Ctx\ncontext(Ctx) fun f() = 5\nfun main() { }\n"
    ));
}

#[test]
fn ubyte_array_rejected() {
    assert!(rejects(
        "fun main() { val a = UByteArray(2); a[0] = 1u; println(a[0]) }\n"
    ));
}
