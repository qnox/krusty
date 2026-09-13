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

/// A generic companion `operator invoke` called by the class name binds its type parameters from the
/// EXPECTED type, exactly as the constructor spelling does. A parameter that appears only in a lambda
/// PARAMETER position (`set: (S, B) -> T`) has no other source: the lambda `{ s, _ -> s }` says
/// nothing about `B`, so without the expectation it stayed unbound and the call's result read
/// `Cell<String, String, X, B>` — rejected against the declared `Cell<String, String, X, X>`.
///
/// This is the optics shape (`Lens<S, A>` = `PLens<S, S, A, A>` built through `PLens`'s companion),
/// which is why one corpus module compiled to nothing.
#[test]
fn a_companion_invoke_binds_type_parameters_from_the_expected_type() {
    let classes = compiles(
        "CompanionInvokeBinding",
        "package demo\n\
        interface Cell<S, T, A, B> {\n\
        \x20 companion object {\n\
        \x20\x20 operator fun <S, T, A, B> invoke(get: (S) -> A, set: (S, B) -> T): Cell<S, T, A, B> =\n\
        \x20\x20\x20 object : Cell<S, T, A, B> {}\n\
        \x20 }\n\
        }\n\
        fun <X> lens(extract: (String) -> X): Cell<String, String, X, X> =\n\
        \x20 Cell(get = { s -> extract(s) }, set = { s, _ -> s })\n",
    );
    assert!(classes
        .iter()
        .any(|class| class == "demo/CompanionInvokeBindingKt"));
}

/// The same shape reached through a typealias that fixes two of the four parameters — how the optics
/// library is actually written (`typealias Lens<S, A> = PLens<S, S, A, A>`), and with a `set` that
/// throws, so the lambda's own type says `Nothing` rather than binding anything.
#[test]
fn a_companion_invoke_through_a_typealias_binds_from_the_expected_type() {
    let classes = compiles(
        "TypealiasCompanionInvoke",
        "package demo\n\
        interface PCell<S, T, A, B> {\n\
        \x20 companion object {\n\
        \x20\x20 operator fun <S, T, A, B> invoke(get: (S) -> A, set: (S, B) -> T): PCell<S, T, A, B> =\n\
        \x20\x20\x20 object : PCell<S, T, A, B> {}\n\
        \x20 }\n\
        }\n\
        typealias Cell<S, A> = PCell<S, S, A, A>\n\
        fun <X> lens(extract: (String) -> X): Cell<String, X> =\n\
        \x20 Cell(\n\
        \x20\x20 get = { source -> extract(source) },\n\
        \x20\x20 set = { _, _ -> throw UnsupportedOperationException(\"read-only\") },\n\
        \x20 )\n",
    );
    assert!(classes
        .iter()
        .any(|class| class == "demo/TypealiasCompanionInvokeKt"));
}

/// The constructor spelling already worked and must keep working.
#[test]
fn a_generic_constructor_still_binds_from_the_expected_type() {
    let classes = compiles(
        "ConstructorBinding",
        "package demo\n\
        class Klass<S, T, A, B>(val get: (S) -> A, val set: (S, B) -> T)\n\
        fun <X> lens(extract: (String) -> X): Klass<String, String, X, X> =\n\
        \x20 Klass(get = { s -> extract(s) }, set = { s, _ -> s })\n",
    );
    assert!(classes.iter().any(|class| class == "demo/Klass"));
}
