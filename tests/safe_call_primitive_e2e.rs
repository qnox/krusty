//! SAFE CALLS on primitive receivers (`x?.foo()` where `x` is `Int`/`Long`/`Int?`/…).
//!
//! Kotlin has no primitive/reference split at the language level, and neither does this lowering:
//! `recv?.m(args)` is a null check around EXACTLY the qualified access — the receiver is
//! re-lowered as the non-null temp (the existing `index_subst` mechanism), so member resolution
//! and emission are the qualified path's own (the prim-op fold, `kotlin/Any` virtuals, array
//! intrinsics, library members, recorded calls, extensions), and the result is boxed into the
//! nullable wrapper when the other branch is `null`. The checker likewise resolves the member
//! against the non-null receiver type with the qualified path's member-first ordering
//! (`check_builtin_operator_method` is shared), so a builtin member beats a same-named extension
//! everywhere.
//!
//! Deliberately still gated (never miscompile): primitive CONVERSIONS through `?.`
//! (`l?.toByte()` — not operator methods), `inc`/`dec`/`mod`/`rangeTo` through `?.` (rejected
//! like their qualified forms), type-parameter receivers erased to `Any`, operator `invoke`
//! through `?.`, and LOCAL functions called through `?.`.

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// A user extension on a NON-NULLABLE primitive receiver: `42?.foo()` is a vacuous safe call —
/// a direct static call whose result kotlinc still types nullable.
#[test]
fn safecall_user_ext_on_nonnull_primitive() {
    run_box(
        r#"
fun Int.foo() = 239
fun Long.bar() = 239.toLong()

fun box(): String {
    42?.foo()
    42.toLong()?.bar()
    return "OK"
}
"#,
        "SafeCallPrimExt",
    );
}

/// The corpus equality shape: `3L == x?.id()` — a user extension on `Long`, compared against a
/// primitive literal (null-safe numeric equality).
#[test]
fn safecall_user_ext_primitive_equality() {
    run_box(
        r#"
fun Long.id() = this

fun doLongReceiver(x: Long) = 3L == x?.id()

fun box(): String {
    if (doLongReceiver(2L)) return "failed 4"
    if (!doLongReceiver(3L)) return "failed 5"
    return "OK"
}
"#,
        "SafeCallPrimEq",
    );
}

/// A builtin arith operator-method through `?.` on a NULLABLE primitive (`i?.plus(3)`): unbox,
/// run the operator, box the result — kotlinc's exact shape.
#[test]
fn safecall_builtin_op_on_nullable_primitive() {
    run_box(
        r#"
fun box(): String {
    val i: Int? = 0
    val j = i?.plus(3)
    if (j != 3) return "f1"
    val n: Int? = null
    if (n?.plus(3) != null) return "f2"
    val l: Long? = 4L
    if (l?.minus(1L) != 3L) return "f3"
    return "OK"
}
"#,
        "SafeCallNullableBuiltin",
    );
}

/// A builtin arith operator-method through `?.` on a NON-NULL primitive (`a?.plus(10)`) — the
/// arith fold emits the plain primitive op (the long-standing deliberate deviation: kotlinc types
/// the `?.` form nullable and rejects most uses).
#[test]
fn safecall_arith_fold_on_nonnull_primitive() {
    run_box(
        r#"
fun box(): String {
    var a = 10
    val r = a?.plus(10)
    if (r != 20) return "f1"
    if (17.div(5) != 3 || 17.rem(5) != 2) return "f2"
    return "OK"
}
"#,
        "SafeCallArithFold",
    );
}

/// A vacuous safe call and a genuinely nullable safe call must select the same library extension
/// after normalizing the receiver to its non-null semantic type. This pair specifically prevents the
/// checker from restoring a primitive-only shortcut that recognizes builtin/source calls but bypasses
/// the ordinary classpath extension index: `takeIf` is generic over its receiver, so whether `Int` is
/// boxed at the null-check boundary is a lowering detail, not a different resolution origin.
#[test]
fn safecall_library_extension_resolution_is_representation_neutral() {
    run_box(
        r#"
fun box(): String {
    val direct = 7?.takeIf { it > 3 }
    if (direct != 7) return "f1"

    val present: Int? = 7
    if (present?.takeIf { it > 3 } != 7) return "f2"

    val absent: Int? = null
    if (absent?.takeIf { it > 3 } != null) return "f3"
    return "OK"
}
"#,
        "SafeCallLibraryExtensionParity",
    );
}

/// A user extension shadowed by the builtin `toString`: the builtin wins (kotlinc: "extension
/// is shadowed by a member") — the result is the REAL `Int.toString()`, not the extension's.
#[test]
fn safecall_shadowed_tostring_uses_builtin() {
    run_box(
        r#"
fun Int.toString(): String = "EXT"

fun box(): String = if (42?.toString() == "42") "OK" else "fail"
"#,
        "SafeCallShadowedToString",
    );
}

/// A user extension read through a NULLABLE-primitive receiver: the null check runs first, the
/// static extension gets the UNBOXED value.
#[test]
fn safecall_user_ext_on_nullable_primitive() {
    run_box(
        r#"
fun Int.foo() = 239

fun box(): String {
    val x: Int? = 42
    if (x?.foo() != 239) return "f1"
    val n: Int? = null
    if (n?.foo() != null) return "f2"
    return "OK"
}
"#,
        "SafeCallNullablePrimExt",
    );
}

/// Generic inline source extensions use the selected call site's receiver for their semantic return
/// type. Recording and substituting that target is shared with qualified calls, so `?.` adds only the
/// nullable outer result and cannot leave the declaration's erased type parameter in `TypeInfo`.
#[test]
fn safecall_generic_source_extension_preserves_call_site_type() {
    run_box(
        r#"
inline fun <T> T.echo(): T = this

fun box(): String {
    val direct: Int = 11.echo()
    if (direct != 11) return "f1"

    val present: Int? = 11
    val safe: Int? = present?.echo()
    if (safe != 11) return "f2"
    return "OK"
}
"#,
        "SafeCallGenericSourceExtension",
    );
}

/// A safe call invoking a FUNCTION-TYPED value (`1L?.b(2L)`): the non-null receiver is the
/// folded-first argument of `Function2.invoke`.
#[test]
fn safecall_receiver_fn_value_invoke() {
    run_box(
        r#"
fun f(b: Long.(Long) -> Long) = 1L?.b(2L)

fun box(): String {
    val x = f { this + it }
    return if (x == 3L) "OK" else "fail $x"
}
"#,
        "SafeCallFnValue",
    );
}

/// A scope function on a non-null primitive receiver (`123?.let { … }`).
#[test]
fn safecall_scope_fn_on_primitive() {
    run_box(
        r#"
fun box(): String {
    val x = 123?.let { it + 1 }
    return if (x == 124) "OK" else "fail $x"
}
"#,
        "SafeCallPrimLet",
    );
}

/// Builtin `Any` methods through `?.` on primitive and `Any` receivers: the checker types them
/// without a record; the lowerer calls the `kotlin/Any.*` intrinsic on the boxed value.
#[test]
fn safecall_any_methods_on_primitive() {
    run_box(
        r#"
fun box(): String {
    val x: Int? = 42
    if (x?.toString() != "42") return "f1"
    val h = x?.hashCode()
    if (h == null || h != 42) return "f2"
    val n: Int? = null
    if (n?.toString() != null) return "f3"
    val a: Any? = null
    if (a?.hashCode() != null) return "f4"
    return "OK"
}
"#,
        "SafeCallAnyMethods",
    );
}

/// Array `.size` through `?.` — the arraylength intrinsic under the null check (KT-14242: must
/// not NPE when the receiver is null).
#[test]
fn safecall_array_size() {
    run_box(
        r#"
var x = 1
fun box(): String {
    val testArray: Array<String?>? = when (1) {
        x -> null
        else -> arrayOfNulls<String>(0)
    }

    val size = testArray?.size

    return size?.toString() ?: "OK"
}
"#,
        "SafeCallArraySize",
    );
}

/// A nullable FUNCTION VALUE receiver through a scope fn (`lambda?.let { it() }`).
#[test]
fn safecall_fun_receiver_scope_let() {
    run_box(
        r#"
var lambda: (() -> String)? = null

fun f() {
    try {
        return
    } finally {
        lambda = { "OK" }
    }
}

fun box(): String {
    f()
    return lambda?.let { it() } ?: "fail"
}
"#,
        "SafeCallFunLet",
    );
}

/// `Char` through `?.`: the only primitive whose stack repr (`int`) and wrapper (`Character`)
/// diverge from a target-type box — the result must narrow to `char` before the merge boxes.
#[test]
fn safecall_char_receiver() {
    run_box(
        r#"
fun box(): String {
    val ch: Char? = 'a'
    if (ch?.plus(1) != 'b') return "f1"
    if (ch?.minus(1) != '`') return "f2"
    if (ch?.code != 97) return "f3"
    val n: Char? = null
    if (n?.plus(1) != null) return "f4"
    return "OK"
}
"#,
        "SafeCallChar",
    );
}

/// `Boolean` bitwise through `?.` on both primitive representations. The nullable cases pin the
/// generic safe-call handoff: the substituted receiver value is a boxed `Boolean?` slot and the
/// primitive bitwise operation must request its semantic `Boolean` operand, which inserts unboxing.
#[test]
fn safecall_boolean_bitwise() {
    run_box(
        r#"
fun box(): String {
    val b = true
    if (b?.and(false) != false) return "f1"
    if (b?.or(false) != true) return "f2"
    if (b?.xor(true) != false) return "f3"
    val present: Boolean? = true
    if (present?.and(true) != true) return "f4"
    val absent: Boolean? = null
    if (absent?.or(true) != null) return "f5"
    return "OK"
}
"#,
        "SafeCallBooleanBitwise",
    );
}

/// Unary operator methods on a nullable primitive: the op runs unboxed, the merge boxes.
#[test]
fn safecall_unary_on_nullable_primitive() {
    run_box(
        r#"
fun box(): String {
    val i: Int? = 5
    if (i?.unaryMinus() != -5) return "f1"
    if (i?.unaryPlus() != 5) return "f2"
    val n: Int? = null
    if (n?.unaryMinus() != null) return "f3"
    return "OK"
}
"#,
        "SafeCallUnaryNullable",
    );
}

/// The exact corpus cases.
#[test]
fn corpus_safecall_primitives_box_ok() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "safeCall/primitive.kt",
        "safeCall/primitiveEqSafeCall.kt",
        "safeCall/primitiveNotEqSafeCall.kt",
        "safeCall/safeCallEqPrimitive.kt",
        "safeCall/safeCallNotEqPrimitive.kt",
        "safeCall/safeCallOnLong.kt",
        "safeCall/kt3430.kt",
        "primitiveTypes/kt239.kt",
        "primitiveTypes/kt518.kt",
        "regressions/arrayLengthNPE.kt",
        "regressions/hashCodeNPE.kt",
        "strings/interpolation.kt",
        "finally/objectInFinally.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must execute successfully, not silently skip"
        );
    }
}

/// REJECTION GUARDS: shapes that must never EMIT. Asserts on the backend outcome, not a run
/// result — a skip and an emitted-but-crashing class both make a run-based check pass, but only
/// the former is acceptable.
#[test]
fn unsupported_safecall_shapes_still_rejected() {
    let jdk = common::jdk_modules();
    let cases: &[(&str, &str)] = &[
        // A primitive CONVERSION through `?.` (`l?.toByte()`) — not an operator method; the
        // builtin-operator resolution doesn't cover conversions.
        (
            "SafeCallConversion",
            r#"
fun box(): String {
    val l: Long? = 230L
    val b = l?.toByte()
    return if (b == (-26).toByte()) "OK" else "fail"
}
"#,
        ),
        // `inc`/`dec` through `?.` — rejected like the qualified form (unmodelled builtin).
        (
            "SafeCallInc",
            r#"
fun box(): String {
    val i: Int? = 0
    return if (i?.inc() == 1) "OK" else "fail"
}
"#,
        ),
        // A user extension SHADOWED by a builtin member (`fun Int.inc`; kotlinc calls the
        // builtin — "extension is shadowed by a member"). Selecting the extension would be a
        // wrong-value miscompile; the builtin itself isn't lowered through `?.` yet, so the
        // call must stay skipped.
        (
            "SafeCallShadowedExt",
            r#"
fun Int.inc(): Int = -1

fun box(): String = if (42?.inc() == 43) "OK" else "fail"
"#,
        ),
        // A type-parameter receiver erased to `Any` (`t?.toInt()` on `T : Number?`) — needs
        // bound-driven member resolution.
        (
            "SafeCallGenericNull",
            r#"
fun <T : Number?> foo(t: T) = t?.toInt()

fun box(): String = if (foo(1) == 1) "OK" else "fail"
"#,
        ),
        // A LOCAL function called through `?.` — the checker records nothing for local targets.
        (
            "SafeCallLocalFn",
            r#"
fun box(): String {
    fun local(x: Int) = x + 1
    val t: Int? = 2
    return if (t?.local(2) == 3) "OK" else "fail"
}
"#,
        ),
        // Function-object Any methods are deliberately unsupported by the qualified path because
        // krusty does not yet preserve the singleton identity/structured string semantics. The generic
        // safe-call lowering must not make the same receiver emittable merely by adding `?.`.
        (
            "SafeCallFunctionAnyMethod",
            r#"
fun box(): String {
    val callback: (() -> Int)? = { 1 }
    return callback?.toString() ?: "OK"
}
"#,
        ),
        (
            "SafeCallFunctionHashCode",
            r#"
fun box(): String {
    val callback: (() -> Int)? = { 1 }
    return if (callback?.hashCode() == null) "OK" else "fail"
}
"#,
        ),
    ];
    for (stem, src) in cases {
        let cp = krusty::toolchain::classpath_jars_for(src);
        let outcome = common::backend_outcome_in_process(src, stem, &cp, Some(jdk.as_path()));
        assert_ne!(
            outcome,
            Some(common::BackendOutcome::Emitted),
            "{stem}: unsupported safe-call shape must not emit (skip, never miscompile)"
        );
    }
}
