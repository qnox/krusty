//! `@JvmStatic` on a property of a named object or a companion, realized as kotlinc does: an
//! object's accessors and backing field are statics of the object class, and a companion keeps its
//! instance accessors while the outer class declares the backing field and a static bridge per
//! accessor. Every emitted class is compared byte for byte with kotlinc's.

use super::common;

const OBJECT: &str = include_str!("box_regressions/jvmStaticObjectProperty.kt");

const COMPANION: &str = include_str!("box_regressions/jvmStaticCompanionProperty.kt");

fn assert_identical(name: &str, source: &str) {
    let classes = common::classes_against_kotlinc_module(&[("Main.kt", source)]);
    let differences = classes.differences();
    assert_eq!(differences, Vec::<String>::new(), "{name}: class files");
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", source)], name);
}

#[test]
fn object_property_is_static() {
    assert_identical("jvm_static_object_property", OBJECT);
}

#[test]
fn companion_property_has_static_bridges() {
    assert_identical("jvm_static_companion_property", COMPANION);
}

const ACCESSOR_LEVEL: &str = include_str!("box_regressions/jvmStaticAccessorProperty.kt");

const SIBLING_USE: &str = r#"fun box(): String {
    Ide.V = 5
    Ide.L = "l"
    H.W = 6
    H.L = "m"
    return if (Ide.A + Ide.V + Ide.C + Ide.L + H.B + H.W + H.C + H.L == "O5clK6cm") "OK" else "fail"
}
"#;

fn assert_files_identical(name: &str, sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    assert_eq!(
        classes.differences(),
        Vec::<String>::new(),
        "{name}: class files"
    );
    common::expect_box_ok_files_with_stdlib(sources, name);
}

#[test]
fn accessor_level_annotations_select_their_accessor() {
    assert_identical("jvm_static_accessor_level", ACCESSOR_LEVEL);
}

#[test]
fn sibling_file_reads_and_writes_static_accessors() {
    let declarations = format!(
        "{}\n{}",
        OBJECT.split("\nfun box").next().unwrap(),
        COMPANION.split("\nfun box").next().unwrap()
    );
    assert_files_identical(
        "jvm_static_property_sibling",
        &[("Declarations.kt", &declarations), ("Main.kt", SIBLING_USE)],
    );
}

/// A companion property's static bridges occupy the outer class's method namespace like a
/// function's, reported at the property (or a written accessor), in source order.
#[test]
fn a_companion_property_bridge_clashing_with_an_outer_member_is_reported_like_kotlinc() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Main.kt",
            "class C {\n\
             \x20   companion object {\n\
             \x20       @JvmStatic val x: Int = 1\n\
             \x20       @JvmStatic var y: String = \"\"\n\
             \x20           set(value) { field = value }\n\
             \x20   }\n\
             \x20   fun getX(): Int = 2\n\
             \x20   fun setY(value: String) {}\n\
             }\n",
        )],
        &[],
    );
}

/// An interface's companion stores its properties itself, so their accessors are compiler
/// defaults with no function behind them; the interface still declares a static bridge for each.
const INTERFACE_COMPANION: &str =
    include_str!("box_regressions/jvmStaticInterfaceCompanionProperty.kt");

#[test]
fn interface_companion_property_has_static_bridges() {
    assert_identical(
        "jvm_static_interface_companion_property",
        INTERFACE_COMPANION,
    );
}

/// The bridge of a compiler-default accessor clashes like the accessor itself, listed after the
/// members it clashes with.
#[test]
fn an_interface_companion_default_accessor_bridge_clash_is_reported_like_kotlinc() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Main.kt",
            "interface C {\n\
             \x20   companion object {\n\
             \x20       @JvmStatic val x: Int = 1\n\
             \x20       @JvmStatic var y: List<String> = listOf()\n\
             \x20   }\n\
             \x20   fun getX(): Int\n\
             \x20   fun setY(v: List<String>)\n\
             }\n",
        )],
        &[],
    );
}
