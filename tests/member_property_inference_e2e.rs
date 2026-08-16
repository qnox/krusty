//! A class-body property whose type comes from a MEMBER call. `private val parts = split(pattern)`
//! next to `private fun split(s: String): List<String>` is ordinary Kotlin — intellij-community's
//! `WordPrefixMatcher` opens with exactly that — but krusty's signature-level inference only knew
//! top-level return types, so it demanded an explicit type. Qualifying the call, annotating the
//! property, or moving the call into a method body all worked, which is what gave the shape away.

use super::common;

fn run(source: &str) -> Option<String> {
    common::compile_and_run_box(
        source,
        "Main",
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    )
}

#[test]
fn a_property_infers_from_an_instance_member_call() {
    let source = r#"
        class Words(pattern: String) {
            private val parts = split(pattern)

            fun count(): Int = parts.size

            private fun split(text: String): List<String> = text.split(" ")
        }

        fun box(): String = "${Words("a b c").count()}"
    "#;

    assert_eq!(run(source).as_deref(), Some("3"));
}

#[test]
fn a_property_infers_from_a_companion_member_call() {
    let source = r#"
        class Words(pattern: String) {
            private val parts = split(pattern)

            fun first(): String = parts[0]

            private companion object {
                private fun split(text: String): List<String> = text.split(" ")
            }
        }

        fun box(): String = Words("x y").first()
    "#;

    assert_eq!(run(source).as_deref(), Some("x"));
}

#[test]
fn a_property_still_infers_from_a_top_level_call() {
    let source = r#"
        fun width(text: String): Int = text.length

        class Label(text: String) {
            val size = width(text)
        }

        fun box(): String = "${Label("four").size}"
    "#;

    assert_eq!(run(source).as_deref(), Some("4"));
}

#[test]
fn an_overloaded_member_does_not_give_the_property_a_wrong_type() {
    let source = r#"
        class C {
            val v = f(42)

            fun f(x: Int): String = "s"
            fun f(x: String): Int = 1

            fun read(): String = v
        }

        fun box(): String = C().read()
    "#;

    assert_eq!(run(source).as_deref(), Some("s"));
}

#[test]
fn an_instance_member_shadows_a_same_named_top_level_function() {
    let source = r#"
        fun choose(x: Int): Int = 99

        class C {
            val v = choose(1)

            fun choose(x: Int): String = "OK"
            fun read(): String = v
        }

        fun box(): String = C().read()
    "#;

    assert_eq!(run(source).as_deref(), Some("OK"));
}

#[test]
fn an_explicit_this_call_uses_the_same_member_candidates() {
    let source = r#"
        class C {
            val v = this.choose(1)

            fun choose(x: Int): String = "OK"
            fun read(): String = v
        }

        fun box(): String = C().read()
    "#;

    assert_eq!(run(source).as_deref(), Some("OK"));
}

#[test]
fn a_generic_member_binds_its_return_from_the_argument() {
    let source = r#"
        class C {
            val v = identity("OK")

            fun <T> identity(value: T): T = value
            fun read(): String = v
        }

        fun box(): String = C().read()
    "#;

    assert_eq!(run(source).as_deref(), Some("OK"));
}

#[test]
fn a_property_infers_from_an_inherited_member_declared_later() {
    let source = r#"
        class Derived : Base() {
            val v = choose(1)
            fun read(): String = v
        }

        open class Base {
            fun choose(x: Int): String = "OK"
        }

        fun box(): String = Derived().read()
    "#;

    assert_eq!(run(source).as_deref(), Some("OK"));
}

#[test]
fn member_property_inference_is_accepted_by_both_frontends() {
    let source = r#"
        fun choose(x: Int): Int = 99

        class C {
            val overloaded = pick(1)
            val shadowed = choose(1)

            fun pick(x: Int): String = "O"
            fun pick(x: String): Int = 1
            fun choose(x: Int): String = "K"
        }
    "#;
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &[common::stdlib_jar()]);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty stdout: {}\nkrusty stderr: {}",
        result.krusty_stdout, result.krusty_stderr
    );
}
