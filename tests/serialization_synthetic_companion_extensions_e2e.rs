//! Extensions declared on the companion the serialization plugin synthesizes.
//!
//! kotlinc's serialization plugin gives every `@Serializable` class without a hand-written
//! companion a generated `Companion`. Its type is an ordinary classifier, so `val T.Companion.x`
//! declares an extension on it, and `T.x` calls that extension on the companion's value, in the
//! declaring file, through an explicit import from another package, and from a dependency.

use super::serialization_test_support::{
    both_compilers_box, both_compilers_box_against_dependency, both_compilers_box_files,
};

const DECLARATIONS: &str = r#"package lib

import kotlinx.serialization.Serializable

@Serializable
class Flag(val value: Boolean = false)

@Serializable
class Name(val value: String = "")

val Flag.Companion.defaultTrue: Flag
    get() = Flag(true)

val Name.Companion.defaultNull: Name
    get() = Name("none")

fun Flag.Companion.of(value: Boolean): Flag = Flag(value)
"#;

#[test]
fn an_extension_on_a_synthesized_companion_resolves_in_its_own_file() {
    let src = r#"import kotlinx.serialization.Serializable

@Serializable
class Flag(val value: Boolean = false)

val Flag.Companion.defaultTrue: Flag
    get() = Flag(true)

fun Flag.Companion.of(value: Boolean): Flag = Flag(value)

fun companion(): Flag.Companion = Flag

fun box(): String =
    "${Flag.defaultTrue.value}|${Flag.of(false).value}|${Flag.Companion.defaultTrue.value}|" +
        "${companion().of(true).value}|${Flag.serializer().descriptor.serialName}"
"#;
    assert_eq!(
        both_compilers_box(src, "synthetic_companion_extension_same_file"),
        "true|false|true|true|Flag"
    );
}

#[test]
fn an_imported_extension_on_a_synthesized_companion_resolves() {
    let main = r#"import lib.Flag
import lib.Name
import lib.defaultNull
import lib.defaultTrue
import lib.of

fun box(): String = "${Flag.defaultTrue.value}|${Name.defaultNull.value}|${Flag.of(false).value}"
"#;
    assert_eq!(
        both_compilers_box_files(
            &[("Lib.kt", DECLARATIONS), ("Main.kt", main)],
            "synthetic_companion_extension_import"
        ),
        "true|none|false"
    );
}

#[test]
fn a_dependency_extension_on_a_synthesized_companion_resolves() {
    let main = r#"import lib.Flag
import lib.Name
import lib.defaultNull
import lib.defaultTrue
import lib.of

fun box(): String = "${Flag.defaultTrue.value}|${Name.defaultNull.value}|${Flag.of(false).value}"
"#;
    assert_eq!(
        both_compilers_box_against_dependency(
            DECLARATIONS,
            main,
            "synthetic_companion_extension_dependency"
        ),
        "true|none|false"
    );
}

#[test]
fn an_extension_on_a_nested_class_synthesized_companion_resolves() {
    let src = r#"import kotlinx.serialization.Serializable

class Outer {
    @Serializable
    class Inner(val value: Int = 0)
}

val Outer.Inner.Companion.seven: Outer.Inner
    get() = Outer.Inner(7)

fun box(): String =
    "${Outer.Inner.seven.value}|${Outer.Inner.serializer().descriptor.serialName}"
"#;
    assert_eq!(
        both_compilers_box(src, "synthetic_companion_extension_nested_class"),
        "7|Outer.Inner"
    );
}
