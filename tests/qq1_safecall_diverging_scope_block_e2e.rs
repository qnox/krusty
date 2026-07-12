//! qq1: a safe call on a scope function whose lambda BLOCK diverges via a non-local `return` (or
//! `throw`) — `x?.let { return it }`. The block body types as `Nothing`, and the safe-call result
//! handler only special-cased `Unit`, so a `Nothing` result fell through to
//! "krusty: safe call (?.) with a non-reference result is not supported". kotlinc types
//! `x?.let { return … }` as `Nothing?` and compiles it. The failure reproduces with NO higher-order
//! receiver at all (a plain `C?`), so the fix is generic to any diverging scope-fn block — keyed on the
//! `Nothing` block/result type, not on `firstOrNull`/`find`/`let` by name. Covers the body-returning
//! (`let`/`run`) and receiver-returning (`also`/`apply`) scope fns, plus the value position
//! (`val r = c?.let { return … }`).
use super::common;

#[test]
fn safe_let_nonlocal_return_on_inline_hof_result() {
    // The exact reported shape: `?.let { return it }` on the nullable result of an inline HOF.
    const SRC: &str = "data class C(val name: String)\n\
        fun f(xs: List<C>): C? {\n\
            xs.firstOrNull { it.name == \"d\" }?.let { return it }\n\
            return null\n\
        }\n\
        fun box(): String {\n\
            val hit = f(listOf(C(\"a\"), C(\"d\"), C(\"b\")))\n\
            val miss = f(listOf(C(\"a\"), C(\"b\")))\n\
            return if (hit?.name == \"d\" && miss == null) \"OK\" else \"F:$hit/$miss\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "qq1_safe_let_hof");
}

#[test]
fn safe_let_nonlocal_return_needs_no_hof_receiver() {
    // Same "non-reference result" failure with a plain `C?` receiver and no HOF anywhere — proof the
    // root cause is the diverging (`Nothing`) block result, not inline-HOF return typing.
    const SRC: &str = "data class C(val name: String)\n\
        fun f(c: C?): C? {\n\
            c?.let { return it }\n\
            return null\n\
        }\n\
        fun box(): String {\n\
            val hit = f(C(\"d\"))\n\
            val miss = f(null)\n\
            return if (hit?.name == \"d\" && miss == null) \"OK\" else \"F:$hit/$miss\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "qq1_safe_let_plain");
}

#[test]
fn safe_run_block_throws_is_nothing() {
    // The other `Nothing` source: a `?.run { throw … }` block. `run` (no params) also routes through
    // the scope-fn result path, so the same generic fix must cover a diverging `throw` body.
    const SRC: &str = "class Box(val v: Int)\n\
        fun f(b: Box?): Int {\n\
            b?.run { throw IllegalStateException(\"boom\") }\n\
            return -1\n\
        }\n\
        fun box(): String {\n\
            val miss = f(null)\n\
            val threw = try { f(Box(1)); false } catch (e: IllegalStateException) { true }\n\
            return if (miss == -1 && threw) \"OK\" else \"F:$miss/$threw\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "qq1_safe_run_throw");
}

#[test]
fn safe_also_nonlocal_return_diverges() {
    // A RECEIVER-returning scope fn (`also`) whose block diverges. Its static result stays the receiver
    // type (a reference), so the `Nothing?` result path does NOT fire — the lowerer detects the diverging
    // block body directly. Without that, the receiver-value load after the `return` leaves a stale stack
    // slot (`VerifyError`).
    const SRC: &str = "class Box(val v: Int)\n\
        fun f(b: Box?): Int {\n\
            b?.also { return it.v }\n\
            return -1\n\
        }\n\
        fun box(): String {\n\
            return if (f(Box(7)) == 7 && f(null) == -1) \"OK\" else \"F\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "qq1_safe_also_return");
}

#[test]
fn safe_apply_nonlocal_return_diverges() {
    // `apply` (receiver is `this`) with a diverging block — same receiver-returning shape as `also`.
    const SRC: &str = "class Box(val v: Int)\n\
        fun f(b: Box?): Int {\n\
            b?.apply { return v }\n\
            return -1\n\
        }\n\
        fun box(): String {\n\
            return if (f(Box(9)) == 9 && f(null) == -1) \"OK\" else \"F\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "qq1_safe_apply_return");
}

#[test]
fn safe_let_diverging_in_value_position() {
    // The diverging safe call used as a VALUE (`val r = c?.let { return … }`), not just a statement — the
    // `Nothing?` result must be a usable (nullable-reference) expression, yielding `null` only when the
    // receiver was null.
    const SRC: &str = "data class C(val name: String)\n\
        fun f(c: C?): C? {\n\
            val r = c?.let { return it }\n\
            return r\n\
        }\n\
        fun box(): String {\n\
            return if (f(C(\"d\"))?.name == \"d\" && f(null) == null) \"OK\" else \"F\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "qq1_safe_let_value");
}

#[test]
fn safe_let_reference_result_still_works() {
    // Regression lock: a scope block that yields an ordinary reference value must keep working — the
    // fix must not broaden into the reference path.
    const SRC: &str = "data class C(val name: String)\n\
        fun f(c: C?): String? = c?.let { it.name }\n\
        fun box(): String {\n\
            return if (f(C(\"d\")) == \"d\" && f(null) == null) \"OK\" else \"F\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "qq1_safe_let_ref");
}
