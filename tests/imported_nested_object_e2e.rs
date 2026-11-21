use super::common;

#[test]
fn imported_nested_object_of_a_companion_reads_its_constant() {
    const DECLARATION: &str = r#"
package sample

interface Bill {
    fun generate()

    companion object {
        const val STEP_ID: String = "sbom"

        object Suppliers {
            const val GOOGLE: String = "Google LLC"
            const val APACHE: String = "The Apache Software Foundation"
        }
    }
}
"#;
    const USE: &str = r#"
package consumer

import sample.Bill.Companion.Suppliers

fun box(): String =
    if (Suppliers.GOOGLE == "Google LLC" && Suppliers.APACHE.startsWith("The Apache")) "OK" else "fail"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Use.kt", USE)],
        "imported nested object of a companion",
    );
}

#[test]
fn imported_nested_object_of_an_object_reads_its_constant() {
    const DECLARATION: &str = r#"
package sample

object Holder {
    object Nested {
        const val VALUE: String = "OK"
    }
}
"#;
    const USE: &str = r#"
package consumer

import sample.Holder.Nested

fun box(): String = Nested.VALUE
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Use.kt", USE)],
        "imported nested object of an object",
    );
}
