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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", &main)], "member_generic_nullable");
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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", &main)], "top_level_generic_nullable");
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
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", &main)], "explicit_targ_nullable");
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
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a nullable result against an 'R : Any' bound: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted a nullable result against a non-null bound: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
