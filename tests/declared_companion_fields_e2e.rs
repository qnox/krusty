//! Companion storage ABI uses the exact name within its authenticated holder.
//! Whole owner metadata is covered separately by the declaration-owned metadata header tests.

use super::common;

const DECLARATIONS: &str = r#"class Owner {
    companion object `Part$Name` {
        val copied: String = token()
        fun token(): String = "OK"
        @JvmStatic fun bridge(): String = token()
    }
}
interface Face {
    companion object `Bits$More` {
        fun token(): String = "OK"
    }
}
"#;

const CALLER: &str = r#"fun box(): String {
    if (Owner.copied != "OK") return "initializer"
    if (Owner.token() != "OK") return "class"
    if (Owner.`Part$Name`.bridge() != "OK") return "bridge"
    if (Face.token() != "OK") return "interface"
    return "OK"
}
"#;

fn assert_storage(sources: &[(&str, &str)], facade: &str) {
    let classes = common::classes_against_kotlinc_module(sources);
    for owner in ["Owner", "Face"] {
        let reference = krusty::jvm::classreader::parse_class(&classes.reference[owner])
            .expect("reference owner class");
        let ours = krusty::jvm::classreader::parse_class(&classes.krusty[owner])
            .expect("emitted owner class");
        assert_eq!(
            ours.fields, reference.fields,
            "complete {owner} field inventory"
        );
    }
    let (reference, ours) =
        classes.method_listing("Owner", "public static final java.lang.String bridge();");
    assert_eq!(ours, reference, "complete outer JVM-static wrapper");
    assert_eq!(
        classes.krusty[facade], classes.reference[facade],
        "complete caller classfile"
    );
    let reference = common::kotlinc_box_files_result(sources, facade);
    assert_eq!(reference, "OK", "reference companion storage behavior");
    common::expect_box_ok_files_with_stdlib(sources, "declared companion storage ABI");
}

#[test]
fn in_file_companion_fields_keep_literal_dollars() {
    let source = format!("{DECLARATIONS}\n{CALLER}");
    assert_storage(&[("Main.kt", &source)], "MainKt");
}

#[test]
fn sibling_companion_fields_keep_literal_dollars() {
    assert_storage(
        &[("Library.kt", DECLARATIONS), ("Main.kt", CALLER)],
        "MainKt",
    );
}
