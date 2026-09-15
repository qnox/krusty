//! An unbounded type parameter of a MEMBER accepts a nullable result.
//!
//! `fun viaExpected(): Status? = wrap { source() }`, where `wrap` is a generic member and `source()`
//! returns `Status?`, was rejected at the lambda body:
//!
//! ```text
//! error: type mismatch: inferred type is Status? but Status was expected
//! ```
//!
//! The member call path synthesizes a generic signature for the callee, and filled each absent type
//! parameter bound with a NON-NULL `Any`. Kotlin's implicit bound for `<R>` is `Any?`. Return-binding
//! inference narrows a nullable candidate whenever the nullable form violates the bound and the
//! non-null form satisfies it — which is exactly what a spurious `Any` bound manufactures. `R` was
//! therefore pinned to `Status`, the lambda's expected result became non-null, and the body failed
//! against it.
//!
//! The same call with an EXPLICIT type argument, and the same generic function declared TOP-LEVEL,
//! both always worked: only the member path builds that synthetic bound.

use super::common;

const DECLARATIONS: &str = "class Status(val v: String)\n\
\n\
fun source(present: Boolean): Status? = if (present) Status(\"here\") else null\n\
\n";

fn both_compilers_box(source: &str, stem: &str) {
    assert_eq!(common::kotlinc_box_result(source), "OK", "kotlinc: {stem}");
    assert_eq!(
        common::expect_box_run_with_stdlib(source, stem),
        "OK",
        "krusty: {stem}"
    );
}

/// The failing shape: a generic MEMBER whose result must bind to a nullable type.
#[test]
fn a_generic_member_binds_its_result_to_a_nullable_type() {
    let main = format!(
        "{DECLARATIONS}\
class Probe {{\n\
\x20   fun pick(present: Boolean): Status? = wrap {{ source(present) }}\n\
\n\
\x20   private fun <R> wrap(block: () -> R): R = block()\n\
}}\n\
fun box(): String {{\n\
\x20   val probe = Probe()\n\
\x20   if (probe.pick(false) != null) return \"FAIL: expected null\"\n\
\x20   val present = probe.pick(true)\n\
\x20   return if (present != null && present.v == \"here\") \"OK\" else \"FAIL: \" + present?.v\n\
}}\n"
    );
    both_compilers_box(&main, "member_generic_nullable");
}

/// The control that isolates the MEMBER path: the identical generic function declared top-level
/// always worked.
#[test]
fn a_top_level_generic_of_the_same_shape_still_works() {
    let main = format!(
        "{DECLARATIONS}\
fun <R> wrap(block: () -> R): R = block()\n\
\n\
class Probe {{\n\
\x20   fun pick(present: Boolean): Status? = wrap {{ source(present) }}\n\
}}\n\
fun box(): String {{\n\
\x20   val probe = Probe()\n\
\x20   if (probe.pick(false) != null) return \"FAIL: expected null\"\n\
\x20   val present = probe.pick(true)\n\
\x20   return if (present != null && present.v == \"here\") \"OK\" else \"FAIL: \" + present?.v\n\
}}\n"
    );
    both_compilers_box(&main, "top_level_generic_nullable");
}

/// The other control: an explicit type argument bypassed the synthetic bound entirely.
#[test]
fn an_explicit_type_argument_still_works() {
    let main = format!(
        "{DECLARATIONS}\
class Probe {{\n\
\x20   fun pick(present: Boolean): Status? = wrap<Status?> {{ source(present) }}\n\
\n\
\x20   private fun <R> wrap(block: () -> R): R = block()\n\
}}\n\
fun box(): String {{\n\
\x20   val probe = Probe()\n\
\x20   if (probe.pick(false) != null) return \"FAIL: expected null\"\n\
\x20   val present = probe.pick(true)\n\
\x20   return if (present != null && present.v == \"here\") \"OK\" else \"FAIL: \" + present?.v\n\
}}\n"
    );
    both_compilers_box(&main, "explicit_targ_nullable");
}

/// The corpus shape retained from #860: a suspend inline member with a crossinline suspend lambda
/// consumes the same declaration-owned generic signature and must accept the nullable result too.
#[test]
fn a_suspend_crossinline_member_admits_a_nullable_binding() {
    let source = "package demo\n\
        class Status(val v: String)\n\
        interface Backend {\n\
        \x20 suspend fun status(name: String): Status?\n\
        }\n\
        class Wrapper(private val inner: Backend) : Backend {\n\
        \x20 override suspend fun status(name: String): Status? = withAuth { inner.status(name) }\n\
        \x20 private suspend inline fun <R> withAuth(\n\
        \x20\x20 crossinline block: suspend () -> R\n\
        \x20 ): R = block()\n\
        }\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[("SuspendInlineBound.kt", source)], &[stdlib]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc accepts the suspend/crossinline nullable binding"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty accepts the same suspend/crossinline nullable binding"
    );
}

/// A kotlinc-built dependency exposes the same empty bound list through metadata. The provider
/// must pass that `GenericSig` through unchanged instead of recreating a member-only bound.
#[test]
fn a_classpath_generic_member_uses_the_same_implicit_bound() {
    let library = "package dep\n\
        class Status(val value: String)\n\
        fun source(present: Boolean): Status? = if (present) Status(\"here\") else null\n\
        class Probe {\n\
        \x20 fun <R> wrap(block: () -> R): R = block()\n\
        }\n";
    let main = "import dep.*\n\
        fun box(): String {\n\
        \x20 val absent: Status? = Probe().wrap { source(false) }\n\
        \x20 val present: Status? = Probe().wrap { source(true) }\n\
        \x20 return if (absent == null && present?.value == \"here\") \"OK\" else \"FAIL\"\n\
        }\n";
    assert_eq!(
        common::kotlinc_box_files_result(&[("Lib.kt", library), ("Main.kt", main)], "MainKt"),
        "OK",
        "kotlinc classpath control"
    );
    assert_eq!(
        common::expect_box_run_against_kotlinc(library, main)
            .expect("compile and run against a kotlinc-built generic member"),
        "OK",
        "krusty consumes the same declaration-owned empty bound"
    );
}

/// A DECLARED non-null bound must still reject a nullable result — the narrowing this restores is
/// legitimate there, and only the absent bound was wrong. Both compilers' output is asserted.
#[test]
fn a_declared_non_null_bound_still_rejects_a_nullable_result() {
    let main = format!(
        "{DECLARATIONS}\
class Probe {{\n\
\x20   fun pick(present: Boolean): Status? = wrap {{ source(present) }}\n\
\n\
\x20   private fun <R : Any> wrap(block: () -> R): R = block()\n\
}}\n"
    );
    let result = common::compiler_diagnostics(&[("Main.kt", &main)], &[]);
    let reference_path = result
        .reference_stderr
        .split(':')
        .next()
        .expect("kotlinc names the rejected file");
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (
            1,
            format!(
                "{reference_path}:6:50: error: return type mismatch: expected 'Status', actual \
                 'Status?'.\n    fun pick(present: Boolean): Status? = wrap {{ source(present) }}\n\
                 \x20                                                ^^^^^^^^^^^^^^^\n"
            )
            .as_str()
        )
    );
    let krusty_path = result
        .krusty_stderr
        .split(':')
        .next()
        .expect("krusty names the rejected file");
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (
            1,
            format!(
                "{krusty_path}:6:48: error: type mismatch: inferred type is Status? but Status was \
                 expected\nkrusty: 1 error(s)\n"
            )
            .as_str()
        )
    );
}
