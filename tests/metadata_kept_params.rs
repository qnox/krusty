//! `Classpath::metadata_kept_params` aligns a classpath function's bytecode candidate to its
//! `@Metadata` SOURCE signature, reporting how many leading descriptor params are REAL (an extension
//! receiver + the source value params). Any params beyond that are synthetic trailing params the
//! descriptor appends (a `@Composable` method's `(Composer, int)`), which the resolver truncates so a
//! source-arity call matches.
//!
//! These tests pin the REGRESSION guard: a normal/`vararg`/overloaded stdlib function must report its
//! FULL descriptor arity (no truncation), so the resolver never drops a real param — an earlier draft
//! truncated `mutableListOf<Int>()`'s `vararg` array down to zero (an empty-arg sibling overload's empty
//! value-param list prefix-matched), emitting an `invokestatic` with too few args → `VerifyError`.

use krusty::jvm::classpath::Classpath;
use krusty::types::Ty;

use super::common;

#[test]
fn vararg_factory_keeps_its_array_param() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    // `mutableListOf(vararg elements: T): MutableList<T>` → descriptor `([Ljava/lang/Object;)…`. The
    // `vararg` is ONE source value param (the array), so the kept count must be 1 — NOT 0 (which would
    // drop the array and underflow the operand stack at the call).
    let params = vec![Ty::array(Ty::obj("java/lang/Object"))];
    let kept =
        cp.metadata_kept_params("kotlin/collections/CollectionsKt", "mutableListOf", &params);
    assert_eq!(
        kept,
        Some(1),
        "the vararg overload's array param must be kept (no truncation), got {kept:?}"
    );
    assert_eq!(
        cp.metadata_kept_params("kotlin/collections/CollectionsKt", "listOf", &params),
        Some(1),
        "listOf's vararg array param must be kept too"
    );
}

#[test]
fn empty_factory_keeps_zero_params() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    // The no-arg `listOf(): List<T>` overload — descriptor `()…`, zero params — aligns at zero kept,
    // which equals its descriptor arity, so the resolver truncates nothing.
    assert_eq!(
        cp.metadata_kept_params("kotlin/collections/CollectionsKt", "listOf", &[]),
        Some(0)
    );
}
