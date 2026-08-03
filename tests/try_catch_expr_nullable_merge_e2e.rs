//! A `try { … } catch { … }` used as an EXPRESSION whose branches are a reference value and a
//! `null` (or otherwise disagreeing reference) branch must merge with the full `join` — `try { x }
//! catch { null }` is `T?`, two different reference classes merge to `Any` — not collapse to `Unit`.
//! The old lenient (statement-position) merge typed every disagreeing pair `Unit`, so
//! `return try { … } catch { null }` in a `T?` function failed "return type mismatch: expected 'T?',
//! actual 'Unit'". Statement-position tries keep the lenient merge. Round-tripped on the JVM.

use super::common;

fn diagnostics(src: &str) -> Option<Vec<String>> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    Some(common::front_end_diagnostics(
        src,
        &[stdlib],
        Some(jdk.as_path()),
    ))
}

#[test]
fn try_catch_expr_merges_value_and_null_to_nullable() {
    // Block-body `return try { v } catch { null }` — the exact shape of a safe-conversion helper.
    const SRC: &str = "fun f(value: String): String? {\n\
    return try {\n\
        value\n\
    } catch (_: IllegalArgumentException) {\n\
        null\n\
    }\n\
}\n\
fun box(): String = if (f(\"x\") == \"x\") \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "TryExprNullBlock");
}

#[test]
fn try_catch_expr_null_merge_expression_body() {
    const SRC: &str = "fun g(value: String): String? = try {\n\
    value\n\
} catch (_: IllegalArgumentException) {\n\
    null\n\
}\n\
fun box(): String = if (g(\"x\") == \"x\") \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "TryExprNullExprBody");
}

#[test]
fn try_catch_expr_null_merge_local_then_return() {
    const SRC: &str = "fun parse(fail: Boolean): String? {\n\
    val r = try {\n\
        if (fail) throw IllegalArgumentException(\"x\") else \"v\"\n\
    } catch (_: IllegalArgumentException) {\n\
        null\n\
    }\n\
    return r\n\
}\n\
fun box(): String = if (parse(false) == \"v\" && parse(true) == null) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "TryExprNullLocal");
}

#[test]
fn try_catch_expr_merges_distinct_references_to_any() {
    // Body and catch produce DIFFERENT reference classes: the expression is `Any`, assignable to an
    // `Any` destination (the emitter frames the merge slot `Object`).
    const SRC: &str = "class Marker(val id: Int)\n\
fun pick(fail: Boolean): Any = try {\n\
    if (fail) throw RuntimeException(\"x\") else \"text\"\n\
} catch (e: RuntimeException) {\n\
    Marker(1)\n\
}\n\
fun box(): String {\n\
    val a = pick(false)\n\
    val b = pick(true)\n\
    if (a !is String || a != \"text\") return \"f1\"\n\
    if (b !is Marker || b.id != 1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "TryExprAnyMerge");
}

#[test]
fn try_catch_distinct_references_preserve_nullable_join() {
    // A full reference join must retain nullability when either input is nullable. Returning the
    // result as non-null `Any` would let a possible `null` escape a source-level non-null contract.
    const SRC: &str = "class Marker\n\
fun pick(value: String?, fail: Boolean): Any? = try {\n\
    if (fail) throw RuntimeException(\"x\") else value\n\
} catch (e: RuntimeException) {\n\
    Marker()\n\
}\n\
fun box(): String = if (pick(null, false) == null && pick(null, true) is Marker) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "TryExprNullableAnyMerge");
}

#[test]
fn try_catch_nullable_distinct_references_reject_non_null_return() {
    // This is the static half of the nullable-`Any` regression above. The expression can produce
    // `null`, so a declared non-null `Any` return must be rejected rather than accepting a latent
    // source-contract violation that only appears at runtime.
    const SRC: &str = "class Marker\n\
fun pick(value: String?, fail: Boolean): Any = try {\n\
    if (fail) throw RuntimeException(\"x\") else value\n\
} catch (e: RuntimeException) {\n\
    Marker()\n\
}\n";
    let Some(messages) = diagnostics(SRC) else {
        return;
    };
    assert!(
        messages
            .iter()
            .any(|message| message.contains("return type mismatch")),
        "expected nullable try result to be rejected as non-null Any, got {messages:?}"
    );
}

#[test]
fn try_catch_statement_keeps_lenient_merge() {
    // Statement position: branches needn't agree (`Int` body vs `String` catch) — no error, no value.
    const SRC: &str = "fun touch(fail: Boolean): Int {\n\
    var hits = 0\n\
    try {\n\
        if (fail) throw RuntimeException(\"x\") else 1\n\
    } catch (e: RuntimeException) {\n\
        hits += 1\n\
        \"recovered\"\n\
    }\n\
    return hits\n\
}\n\
fun box(): String = if (touch(false) == 0 && touch(true) == 1) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "TryStmtLenient");
}
