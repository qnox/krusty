//! The `Companion` the serialization plugin adds to a `@Serializable` class that declares none is a
//! nested classifier like any other: a type position, an extension receiver and an import can name
//! it, so an extension declared on `Type.Companion` resolves for `Type.member` at its use sites.
//! krusty only knew the name once the class's members were collected, so a spelling resolved while
//! the type universe was built (a receiver `Type.Companion`) found no such classifier.

use super::serialization_test_support::both_compilers_box_files;

#[test]
fn an_extension_on_a_generated_companion_resolves_across_files() {
    let types = r#"package dsl

import kotlinx.serialization.Serializable

@Serializable
class FlagType(val isNullable: Boolean = true)

@Serializable
class NameType(val text: String = "")
"#;
    let utils = r#"package dsl

val FlagType.Companion.defaultFalse: FlagType
    get() = FlagType(isNullable = false)

val NameType.Companion.defaultNull: NameType
    get() = NameType()
"#;
    let main = r#"import dsl.FlagType
import dsl.NameType
import dsl.defaultFalse
import dsl.defaultNull

fun describe(companion: FlagType.Companion): FlagType = companion.defaultFalse

fun box(): String {
    val flag = FlagType.defaultFalse
    val name = NameType.defaultNull
    val viaValue = describe(FlagType.Companion)
    return "${flag.isNullable}|${name.text.isEmpty()}|${viaValue.isNullable}"
}
"#;
    assert_eq!(
        both_compilers_box_files(
            &[("Types.kt", types), ("Utils.kt", utils), ("Main.kt", main)],
            "serializable_generated_companion_name",
        ),
        "false|true|false"
    );
}
