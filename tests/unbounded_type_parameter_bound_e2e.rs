use super::common;

fn compiles(name: &str, source: &str) -> Vec<String> {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    common::compile_in_process_files(
        &[(name, source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .unwrap_or_else(|| panic!("{name}: krusty rejected source kotlinc accepts"))
    .into_iter()
    .map(|(class, _)| class)
    .collect()
}

/// An UNBOUNDED method type parameter's implicit bound is `Any?`, not `Any`. The member lambda plan
/// built it non-null, so binding `R` from a nullable expected type failed that bound and inference
/// narrowed it: `fun f(): Status? = wrap { source() }` shaped the lambda's target as `() -> Status`
/// and rejected a body legitimately returning `Status?`. Explicit type arguments bypassed the plan
/// and always worked, which is what pinned the bound as the cause.
#[test]
fn an_unbounded_method_type_parameter_admits_a_nullable_binding() {
    let classes = compiles(
        "UnboundedTypeParameter",
        "package demo\n\
        class Status(val v: String)\n\
        fun source(): Status? = null\n\
        class Probe {\n\
        \x20 fun viaExpected(): Status? = wrap { source() }\n\
        \x20 fun explicitArg(): Status? = wrap<Status?> { source() }\n\
        \x20 private fun <R> wrap(block: () -> R): R = block()\n\
        }\n",
    );
    assert!(classes.iter().any(|class| class == "demo/Probe"));
}

/// The same through an `inline` helper and through a `suspend inline` one taking a `crossinline`
/// suspend lambda — the shape a corpus module uses to wrap every backend call.
#[test]
fn a_suspend_inline_helper_admits_a_nullable_binding() {
    let classes = compiles(
        "SuspendInlineBound",
        "package demo\n\
        class Status(val v: String)\n\
        interface Backend {\n\
        \x20 suspend fun status(name: String): Status?\n\
        }\n\
        class Wrapper(private val inner: Backend) : Backend {\n\
        \x20 override suspend fun status(name: String): Status? = withAuth { inner.status(name) }\n\
        \x20 private suspend inline fun <R> withAuth(crossinline block: suspend () -> R): R =\n\
        \x20\x20 block()\n\
        }\n",
    );
    assert!(classes.iter().any(|class| class == "demo/Wrapper"));
}

/// A DECLARED non-null bound still rejects a nullable argument — the implicit bound becoming `Any?`
/// must not make every type parameter nullable-accepting.
#[test]
fn a_declared_non_null_bound_still_rejects_null() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package demo\n\
        class Status(val v: String)\n\
        fun source(): Status? = null\n\
        class Probe {\n\
        \x20 fun viaExpected(): Status? = wrap { source() }\n\
        \x20 private fun <R : Any> wrap(block: () -> R): R = block()\n\
        }\n";
    assert!(
        common::compile_in_process_files(
            &[("DeclaredBound", source)],
            &[stdlib, jdk.clone()],
            Some(jdk.as_path()),
        )
        .is_none(),
        "`R : Any` must still reject a nullable binding"
    );
}
