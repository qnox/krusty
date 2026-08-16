//! A property whose INITIALIZER starts on the following line.
//!
//! Kotlin's property grammar is `… (NL* '=' NL* expression)?`, so a declaration whose type
//! annotation fills the line may put its `=` on the next one — which is what a formatter does to a
//! long generic type. Stopping the declaration at the newline leaves the `=` to be read as the start
//! of the next declaration, and the file fails to parse with a diagnostic that names the enclosing
//! body rather than the property ("object bodies support 'fun', 'val'/'var', and 'init' blocks",
//! once per token of the initializer).
use super::common;

#[test]
fn an_initializer_on_the_next_line_belongs_to_its_property() {
    const MAIN: &str = "package repro\n\
        object Spec {\n\
            val mapped: Map<String, Int>\n\
                = mapOf(\"x\" to 1)\n\
            val plain: String = \"s\"\n\
        }\n\
        class Holder {\n\
            val items: List<Int>\n\
                = listOf(1, 2)\n\
        }\n\
        val topLevel: String\n\
            = \"top\"\n\
        fun box(): String {\n\
            val local: Int\n\
                = 7\n\
            if (Spec.mapped[\"x\"] != 1) return \"fail object\"\n\
            if (Spec.plain != \"s\") return \"fail sibling\"\n\
            if (Holder().items.size != 2) return \"fail class\"\n\
            if (topLevel != \"top\") return \"fail top level\"\n\
            if (local != 7) return \"fail local\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "initializer on the next line");
}

#[test]
fn a_line_break_inside_a_local_declaration_is_a_continuation() {
    // The statement arm reads the same grammar: a local declaration may break after the colon or
    // before the `=`, and so may a destructuring one and a `when` subject binding.
    const MAIN: &str = "package repro\n\
        fun box(): String {\n\
            val local:\n\
                Map<String, Int> = mapOf(\"a\" to 1)\n\
            val (first, second)\n\
                = Pair(3, 4)\n\
            val subject = when (val value:\n\
                Int = local[\"a\"] ?: 0) {\n\
                1 -> \"one\"\n\
                else -> \"other\" + value\n\
            }\n\
            if (local[\"a\"] != 1) return \"fail local\"\n\
            if (first + second != 7) return \"fail destructuring\"\n\
            if (subject != \"one\") return \"fail subject\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "line break inside a local declaration",
    );
}

#[test]
fn a_semicolon_still_ends_the_declaration() {
    // The lexer spells a line break and an explicit `;` as the same token, but Kotlin's `NL*` is a
    // line break only: `val a: Int; = 1` is two declarations, the second of which is not one. Reading
    // past the semicolon would ACCEPT a program kotlinc rejects — the reason this looks past newlines
    // through the helper that stops at `;` rather than through a plain token loop.
    for source in [
        "val a: Int; = 1\n",
        "class C { val a:; Int = 1 }\n",
        "fun f() { val a: Int; = 1 }\n",
    ] {
        let diagnostics = common::front_end_diagnostics_files_with_stdlib(&[source]);
        assert!(
            !diagnostics.is_empty(),
            "a semicolon must end the declaration: {source:?} produced no diagnostic"
        );
    }
}

#[test]
fn a_deferred_property_is_still_deferred() {
    // The mirror case: `val x: T` with no initializer anywhere keeps its deferred-assignment
    // meaning. Looking past the newlines must not invent an initializer out of the next statement.
    const MAIN: &str = "package repro\n\
        fun box(): String {\n\
            val deferred: String\n\
            deferred = \"assigned\"\n\
            var counter: Int\n\
            counter = 2\n\
            counter += 1\n\
            return if (deferred == \"assigned\" && counter == 3) \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "deferred property");
}

#[test]
fn a_type_annotation_on_the_next_line_belongs_to_its_property() {
    // The same rule one token earlier: `… (':' NL* type)?`. A formatter breaking after the colon
    // produced the same unparseable position as breaking before the `=`.
    const MAIN: &str = "package repro\n\
        object Spec {\n\
            val wrapped:\n\
                Map<String, Int> =\n\
                mapOf(\"x\" to 1)\n\
            val plain: String = \"s\"\n\
        }\n\
        class Holder {\n\
            val items:\n\
                List<Int>\n\
                = listOf(1, 2)\n\
        }\n\
        fun box(): String {\n\
            if (Spec.wrapped[\"x\"] != 1) return \"fail wrapped\"\n\
            if (Spec.plain != \"s\") return \"fail sibling\"\n\
            if (Holder().items.size != 2) return \"fail class\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MAIN)],
        "type annotation on the next line",
    );
}
