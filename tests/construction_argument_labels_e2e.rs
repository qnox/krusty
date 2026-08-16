//! Argument LABELS map onto constructor parameter slots.
//!
//! A construction's parameter shapes live in declaration order while its arguments are written in
//! source order, and labels are free to reorder them. The parameter each argument fills is
//! therefore a mapping, never its written position: reading the shapes by position hands each
//! lambda another parameter's function type, which compiles perfectly well and throws
//! `ClassCastException` at run time. These tests RUN `box()` — a compile-only assertion cannot see
//! the defect at all.
use super::common;

#[test]
fn swapped_labels_on_concrete_function_parameters_keep_their_own_lambda() {
    // Nothing generic in sight: the two lambdas simply swap places against the declaration.
    const MAIN: &str = "package repro\n\
        class Pair2(val f: (String) -> String, val g: (Int) -> String)\n\
        fun box(): String {\n\
            val p = Pair2(g = { n -> \"i\" + n }, f = { s -> \"s\" + s })\n\
            if (p.f(\"a\") != \"sa\") return \"fail f\"\n\
            if (p.g(2) != \"i2\") return \"fail g\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "swapped labels on concrete function parameters",
    );
}

#[test]
fn labels_out_of_declaration_order_bind_their_own_parameter() {
    // The same mapping through the module provider, where the generic classifier is in another file.
    const LIB: &str = "package repro\n\
        class Conv<A, B>(val f: (A) -> String, val g: (B) -> String, val a: A, val b: B)\n";
    const MAIN: &str = "package repro\n\
        fun box(): String {\n\
            val c = Conv(g = { n -> n.toString() }, f = { s -> s.toString() }, a = \"abc\", b = 7)\n\
            if (c.f(c.a) != \"abc\") return \"fail f\"\n\
            if (c.g(c.b) != \"7\") return \"fail g\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "labels out of declaration order",
    );
}

#[test]
fn a_local_class_uses_the_same_constructor_mapping() {
    const MAIN: &str = "fun box(): String {\n\
        class Local(val f: (String) -> String, val g: (Int) -> String)\n\
        val value = Local(g = { n -> \"i\" + n }, f = { s -> \"s\" + s })\n\
        return if (value.f(\"a\") == \"sa\" && value.g(2) == \"i2\") \"OK\" else \"fail\"\n\
    }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "labels on a local classifier constructor",
    );
}

#[test]
fn a_nested_class_uses_the_same_constructor_mapping() {
    const MAIN: &str = "class Outer {\n\
        class Nested(val f: (String) -> String, val g: (Int) -> String)\n\
    }\n\
    fun box(): String {\n\
        val value = Outer.Nested(g = { n -> \"i\" + n }, f = { s -> \"s\" + s })\n\
        return if (value.f(\"a\") == \"sa\" && value.g(2) == \"i2\") \"OK\" else \"fail\"\n\
    }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "labels on a nested classifier constructor",
    );
}

#[test]
fn a_label_beside_a_trailing_lambda_keeps_both_shapes() {
    // `Ctor(named = …) { trailing }` is ordinary Kotlin: the trailing lambda is written last and
    // belongs to the last parameter, so the label costs the mapping nothing. Declining the mapping
    // instead leaves the lambda unshaped and the whole file fails to lower.
    const MAIN: &str = "package repro\n\
        class Scope {\n\
            val out = StringBuilder()\n\
            fun add(s: String) { out.append(s) }\n\
        }\n\
        class Holder(val name: String, val body: (Int) -> String)\n\
        class Built(val name: String, val body: Scope.() -> Unit)\n\
        fun box(): String {\n\
            val h = Holder(name = \"y\") { n -> \"p\" + n }\n\
            if (h.name + h.body(3) != \"yp3\") return \"fail holder\"\n\
            val b = Built(name = \"x\") { add(\"hi\") }\n\
            val scope = Scope()\n\
            b.body(scope)\n\
            if (b.name + \":\" + scope.out != \"x:hi\") return \"fail built\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "a label beside a trailing lambda",
    );
}

#[test]
fn labels_map_onto_a_dependency_declaration_too() {
    // The other origin: the parameter names come from the dependency's `@Metadata` rather than a
    // module signature. Both call sites map labels, so neither origin reads a shape by position.
    const LIB: &str = "package lib\n\
        class Conv(val f: (String) -> String, val g: (Int) -> String)\n";
    const MAIN: &str = "import lib.Conv\n\
        fun box(): String {\n\
            val c = Conv(g = { n -> \"i\" + n }, f = { s -> \"s\" + s })\n\
            if (c.f(\"a\") != \"sa\") return \"fail f\"\n\
            if (c.g(2) != \"i2\") return \"fail g\"\n\
            return \"OK\"\n\
        }\n";
    let Some(build) =
        common::compile_libs_build("construction_labels_classpath", &[("Lib.kt", LIB)])
    else {
        return;
    };
    let Some(library) = build.reference_out() else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let result = common::expect_box_run(
        MAIN,
        "Main",
        &[library.to_path_buf(), stdlib],
        Some(jdk.as_path()),
    );
    assert_eq!(result, "OK", "labels against a dependency declaration");
}
