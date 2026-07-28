use super::common;

#[test]
fn imported_companion_function_properties_typecheck_in_defaults() {
    const SOURCE: &str = r#"
package sample

import sample.Selection.Companion.primaryRule
import sample.Selection.Companion.secondaryRule

interface Subject

interface Consumer {
    fun choose(
        primary: (Subject) -> Selection = primaryRule,
        secondary: (Subject) -> Selection = secondaryRule,
    ): Selection
}

class Selection {
    companion object {
        val primaryRule: (Subject) -> Selection = { Selection() }
        val secondaryRule: (Subject) -> Selection = { Selection() }
    }
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "imported companion function properties",
    );
}

#[test]
fn imported_nested_companion_property_beats_unrelated_top_level() {
    const DECLARATION: &str = r#"
package source

class Outer {
    class Holder {
        companion object {
            val fragment: String = "O"
        }
    }
}
"#;
    const COLLISION: &str = r#"
package unrelated

val selected: String = "wrong"
"#;
    const USE: &str = r#"
package consumer

import source.Outer.Holder.Companion.fragment as selected

fun value(suffix: String = "K"): String = selected + suffix
fun box(): String = value()
"#;

    common::expect_box_ok_files_with_stdlib(
        &[
            ("Declaration.kt", DECLARATION),
            ("Collision.kt", COLLISION),
            ("Use.kt", USE),
        ],
        "imported nested companion property",
    );
}

#[test]
fn private_companion_property_import_is_rejected() {
    const SOURCE: &str = r#"
package sample

import sample.Holder.Companion.secret

class Holder {
    companion object {
        private val secret: String = "hidden"
    }
}

fun reveal(): String = secret
"#;

    let diagnostics = common::front_end_diagnostics_files(&[SOURCE], &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("cannot access 'secret': it is private")),
        "expected private companion import diagnostic, got {diagnostics:?}"
    );
}
