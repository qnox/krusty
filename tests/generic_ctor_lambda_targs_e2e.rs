//! A generic class whose CONSTRUCTOR takes a function over the class's own type parameters.
//!
//! `class Store<ROOT, DOMAIN>(wrapper: Wrapper<ROOT>, empty: ROOT, toDomain: (ROOT) -> DOMAIN,
//! toRoot: (DOMAIN) -> ROOT)` supplies that parameter as the lambda's expectation. The expectation
//! is the DECLARATION's template, so the type arguments the call already fixes — an explicit
//! `Store<A, B>(…)`, the expected result, and the arguments that are not themselves contextual —
//! must be substituted into it before the lambda body is checked. Handing the lambda the raw
//! `(ROOT) -> DOMAIN` types its parameter as the type variable's erased bound: every member read in
//! the body is "unresolved reference", the failed body contributes nothing back, and the
//! construction collapses to `Store<Any, Any>` so every later call on the value cascades.
//!
//! The lambda's RESULT is the other direction of the same inference: `DOMAIN` is visible nowhere
//! else, so the checked body has to feed it back.
use super::common;

const STORE: &str = "package repro\n\
    class Wrapper<T>(val name: String)\n\
    class Store<ROOT, DOMAIN>(\n\
        private val wrapper: Wrapper<ROOT>,\n\
        private val empty: ROOT,\n\
        private val toDomain: (ROOT) -> DOMAIN,\n\
        private val toRoot: (DOMAIN) -> ROOT,\n\
    ) {\n\
        fun domain(): DOMAIN = toDomain(empty)\n\
        fun roundTrip(domain: DOMAIN): ROOT = toRoot(domain)\n\
        fun label(): String = wrapper.name\n\
    }\n";

const TYPES: &str = "package repro\n\
    class Entry(val id: String)\n\
    class RootDto(val entries: List<Entry>)\n";

#[test]
fn a_constructor_lambda_takes_its_parameter_from_explicit_type_arguments() {
    const MAIN: &str = "package repro\n\
        fun box(): String {\n\
            val store =\n\
                Store<RootDto, List<Entry>>(\n\
                    Wrapper(\"local\"),\n\
                    RootDto(listOf(Entry(\"a\"))),\n\
                    { dto -> dto.entries },\n\
                    { list -> RootDto(list) },\n\
                )\n\
            if (store.domain().size != 1) return \"fail size\"\n\
            if (store.roundTrip(listOf(Entry(\"c\"))).entries.single().id != \"c\") return \"fail trip\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Store.kt", STORE), ("Types.kt", TYPES), ("Main.kt", MAIN)],
        "ctor lambda parameter from explicit type arguments",
    );
}

#[test]
fn a_constructor_lambda_takes_its_parameter_from_the_expected_result() {
    const MAIN: &str = "package repro\n\
        fun box(): String {\n\
            val store: Store<RootDto, List<Entry>> =\n\
                Store(\n\
                    Wrapper(\"expected\"),\n\
                    RootDto(listOf(Entry(\"a\"))),\n\
                    { dto -> dto.entries },\n\
                    { list -> RootDto(list) },\n\
                )\n\
            return if (store.domain().single().id == \"a\") \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Store.kt", STORE), ("Types.kt", TYPES), ("Main.kt", MAIN)],
        "ctor lambda parameter from expected result",
    );
}

#[test]
fn a_constructor_lambda_takes_its_parameter_from_a_sibling_argument() {
    // `ROOT` is pinned only by the `Wrapper<RootDto>` argument — a concrete argument binds it
    // before the contextual one is checked.
    const MAIN: &str = "package repro\n\
        fun box(): String {\n\
            val store =\n\
                Store(\n\
                    wrapper = Wrapper<RootDto>(\"local\"),\n\
                    empty = RootDto(listOf(Entry(\"a\"), Entry(\"b\"))),\n\
                    toDomain = { dto -> dto.entries },\n\
                    toRoot = { list -> RootDto(list) },\n\
                )\n\
            if (store.domain().size != 2) return \"fail size\"\n\
            if (store.label() != \"local\") return \"fail label\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Store.kt", STORE), ("Types.kt", TYPES), ("Main.kt", MAIN)],
        "ctor lambda parameter from a sibling argument",
    );
}

#[test]
fn a_constructor_type_parameter_binds_from_the_lambda_result() {
    // `DOMAIN` appears only in the lambda's return position: the checked body is its one constraint.
    const MAIN: &str = "package repro\n\
        class Counted<ROOT, DOMAIN>(val wrapper: Wrapper<ROOT>, val toDomain: (ROOT) -> DOMAIN)\n\
        fun box(): String {\n\
            val counted = Counted(Wrapper<RootDto>(\"c\")) { dto -> dto.entries.size }\n\
            val size: Int = counted.toDomain(RootDto(listOf(Entry(\"a\"), Entry(\"b\"))))\n\
            return if (size == 2) \"OK\" else \"fail size\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Store.kt", STORE), ("Types.kt", TYPES), ("Main.kt", MAIN)],
        "ctor type parameter from the lambda result",
    );
}

#[test]
fn contextual_arguments_are_typed_after_what_binds_them() {
    // Written in the order that does not work: `toRoot: (DOMAIN) -> ROOT` can only be checked once
    // `DOMAIN` is known, and `DOMAIN` appears nowhere but the RESULT of `toDomain`.
    const MAIN: &str = "package repro\n\
        fun box(): String {\n\
            val store =\n\
                Store(\n\
                    toRoot = { list -> RootDto(list) },\n\
                    toDomain = { dto -> dto.entries },\n\
                    empty = RootDto(listOf(Entry(\"a\"))),\n\
                    wrapper = Wrapper<RootDto>(\"ordered\"),\n\
                )\n\
            if (store.domain().single().id != \"a\") return \"fail domain\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Store.kt", STORE), ("Types.kt", TYPES), ("Main.kt", MAIN)],
        "contextual arguments typed in dependency order",
    );
}

#[test]
fn a_local_generic_constructor_uses_the_same_lambda_substitution() {
    const MAIN: &str = "package repro\n\
        class Entry(val id: String)\n\
        fun box(): String {\n\
            class Local<T>(val value: T, val render: (T) -> String)\n\
            val local = Local(Entry(\"local\")) { entry -> entry.id }\n\
            return if (local.render(local.value) == \"local\") \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "local generic constructor lambda substitution",
    );
}

#[test]
fn a_value_class_type_argument_remains_a_semantic_lambda_parameter() {
    const MAIN: &str = "package repro\n\
        @JvmInline value class Token(val text: String)\n\
        class Project<T>(val value: T, val render: (T) -> String)\n\
        fun box(): String {\n\
            val project = Project(Token(\"OK\")) { token -> token.text }\n\
            return project.render(project.value)\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "value-class constructor lambda substitution",
    );
}

#[test]
fn a_classpath_constructor_lambda_takes_its_parameter_from_a_sibling_argument() {
    // The same class reached as a DEPENDENCY: one substitution path, not a per-provenance one.
    const LIB: &str = "package lib\n\
        class Wrapper<T>(val name: String)\n\
        class Store<ROOT, DOMAIN>(\n\
            private val wrapper: Wrapper<ROOT>,\n\
            private val empty: ROOT,\n\
            private val toDomain: (ROOT) -> DOMAIN,\n\
        ) {\n\
            fun domain(): DOMAIN = toDomain(empty)\n\
            fun label(): String = wrapper.name\n\
        }\n";
    const MAIN: &str = "import lib.Store\n\
        import lib.Wrapper\n\
        class Entry(val id: String)\n\
        class RootDto(val entries: List<Entry>)\n\
        fun box(): String {\n\
            val store =\n\
                Store(\n\
                    wrapper = Wrapper<RootDto>(\"dep\"),\n\
                    empty = RootDto(listOf(Entry(\"a\"))),\n\
                    toDomain = { dto -> dto.entries },\n\
                )\n\
            if (store.domain().single().id != \"a\") return \"fail domain\"\n\
            if (store.label() != \"dep\") return \"fail label\"\n\
            return \"OK\"\n\
        }\n";
    let build = common::compile_libs_build("generic_ctor_lambda_classpath", &[("Lib.kt", LIB)])
        .expect("scratch directory for constructor dependency");
    let Some(reference) = build.reference_out() else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let result = common::expect_box_run(
        MAIN,
        "Main",
        &[reference.to_path_buf(), stdlib],
        Some(jdk.as_path()),
    );
    assert_eq!(result, "OK", "classpath generic ctor lambda parameter");
}
