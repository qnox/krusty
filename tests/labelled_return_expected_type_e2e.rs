use super::common;

/// A `return@label` to a lambda is typed against the lambda's EXPECTED return type, exactly like a
/// `return` from a function. Without it a bare `emptyList()` in an early return stayed
/// `List<Nothing>`, joined with the tail's `List<Long>` to `Any`, and the whole file was rejected —
/// where kotlinc infers the element type from the expectation and accepts.
#[test]
fn a_labelled_return_takes_the_lambdas_expected_type() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package demo\n\
        inline fun <T> scope(block: () -> T): T = block()\n\
        class Agents {\n\
        \x20 fun stale(flag: Boolean, other: Boolean): List<Long> =\n\
        \x20\x20 scope {\n\
        \x20\x20\x20 if (flag) return@scope emptyList()\n\
        \x20\x20\x20 if (other) return@scope emptyList()\n\
        \x20\x20\x20 listOf(1L)\n\
        \x20\x20 }\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("LabelledReturn", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles a labelled return whose value needs the expected type");
    assert!(
        classes.iter().any(|(name, _)| name == "demo/Agents"),
        "expected demo/Agents among {:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
}

/// The same rule for a labelled return in a lambda whose expected type comes from a declared
/// function-typed parameter rather than a type variable.
#[test]
fn a_labelled_return_to_a_declared_function_type_takes_its_expected_type() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package demo\n\
        fun run(block: () -> List<Long>): List<Long> = block()\n\
        class Runner {\n\
        \x20 fun pick(flag: Boolean): List<Long> =\n\
        \x20\x20 run {\n\
        \x20\x20\x20 if (flag) return@run emptyList()\n\
        \x20\x20\x20 listOf(2L)\n\
        \x20\x20 }\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("LabelledReturnDeclared", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles a labelled return against a declared function type");
    assert!(
        classes.iter().any(|(name, _)| name == "demo/Runner"),
        "expected demo/Runner among {:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
}
