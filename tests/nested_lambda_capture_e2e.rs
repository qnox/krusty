//! A NESTED lambda capturing a variable from 2+ levels out (`host { inner { x = outer } }`) lowered to
//! `lower None` (skipped): `lower_lambda_sam`'s capture detection stopped at a nested lambda, so the
//! outer closure never captured the transitively-used variable. Now a CLOSURE lambda captures through
//! nested lambdas — while an INLINE-spliced lambda keeps shallow captures (it accesses the variable
//! directly). Round-tripped on a real JVM.

use super::common;

#[test]
fn nested_closure_capture_runs() {
    // `host`/`inner` are NON-inline (real closures); the inner lambda captures `outer` two levels out.
    const SRC: &str = "fun host(b: () -> Unit) { b() }\n\
        fun inner(f: () -> Unit) { f() }\n\
        fun box(): String {\n\
        \x20 var x = \"\"\n\
        \x20 val outer = \"OK\"\n\
        \x20 host { inner { x = outer } }\n\
        \x20 return x\n\
        }\n";
    common::assert_box_ok_with_stdlib(SRC, "N");
}
