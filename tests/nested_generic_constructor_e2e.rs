//! A nested generic class constructor is available by its simple name inside the enclosing class, and
//! its type argument is inferred from the constructor arguments.

use super::common;

#[test]
fn nested_generic_constructor_infers_argument_type() {
    const SOURCE: &str = r#"
        class Token(val text: String)

        class Registry {
            fun List<Token?>.make(): Any =
                groupBy { token -> Key("item", true, token) }

            private data class Key<T>(
                val label: String,
                val enabled: Boolean,
                val value: T,
            )
        }
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics, got: {diagnostics:?}"
    );
}

#[test]
fn nested_generic_constructor_runs_from_member_extension() {
    const SOURCE: &str = r#"
        class Token(val text: String)

        class Registry {
            private fun Token.wrap(): String {
                val key = Key(text, true, this)
                return text
            }

            fun read(token: Token): String = token.wrap()

            private data class Key<T>(
                val label: String,
                val enabled: Boolean,
                val value: T,
            )
        }

        fun box(): String = Registry().read(Token("OK"))
    "#;

    common::expect_box_ok_with_stdlib(SOURCE, "S");
}

#[test]
fn extension_receiver_does_not_create_classifier_scope() {
    const SOURCE: &str = r#"
        class Container {
            class Item {
                fun text(): String = "nested"
            }
        }

        class Item {
            fun text(): String = "OK"
        }

        fun Container.make(): String = Item().text()

        fun box(): String = Container().make()
    "#;

    common::expect_box_ok_with_stdlib(SOURCE, "S");
}

#[test]
fn extension_receiver_nested_type_requires_qualification() {
    const SOURCE: &str = r#"
        class Container {
            class Item
        }

        fun Container.make(): Any {
            val value: Item = TODO()
            return value
        }
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "unresolved reference 'Item'."),
        "expected the unqualified nested type to stay unresolved, got: {diagnostics:?}"
    );
}

#[test]
fn extension_receiver_nested_constructor_requires_qualification() {
    const SOURCE: &str = r#"
        class Container {
            class Item
        }

        fun Container.make(): Any = Item()
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "unresolved function 'Item'"),
        "expected the unqualified nested constructor to stay unresolved, got: {diagnostics:?}"
    );
}
