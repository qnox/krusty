//! A `@Serializable` class property whose type is an enum that is not itself `@Serializable` and
//! that the file does not declare: one from another file of the module or from a dependency.
//!
//! kotlinc's plugin builds the enum's serializer at the use site, with the runtime's enum factory
//! over the enum's entries and each entry's `@SerialName`. krusty knew an entry's annotations only
//! for an enum of the same file, so such a file was rejected as "not yet supported by the IR
//! backend".
//!
//! PLATFORM-SPECIFIC: krusty's serialization plugin builds JVM runtime calls against the ABI of
//! `kotlinx-serialization-core-jvm`, and no box lane provides a serialization runtime KLIB, so
//! these programs run here and not from `tests/box_regressions/`.
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};
use super::serialization_test_support::{
    both_compilers_box_against_dependency, both_compilers_box_files, reference_dependency,
};

const DECLARATIONS: &str = "import kotlinx.serialization.SerialName\n\
    \n\
    enum class Mode { FAST, SLOW }\n\
    enum class Named { @SerialName(\"x\") X, Y }\n\
    @SerialName(\"renamed.Level\") enum class Level { LOW, HIGH }\n";

const DEPENDENCY: &str = "package dep\n\
    \n\
    import kotlinx.serialization.SerialName\n\
    \n\
    enum class Mode { FAST, SLOW }\n\
    enum class Named { @SerialName(\"x\") X, Y }\n\
    @SerialName(\"renamed.Level\") enum class Level { LOW, HIGH }\n";

const CONSUMER: &str = "import dep.*\n\
    import kotlinx.serialization.Serializable\n\
    \n\
    @Serializable\n\
    data class Holder(val mode: Mode, val named: Named, val level: Level?, val modes: List<Mode>)\n";

fn box_program(holder_import: &str) -> String {
    format!(
        "{holder_import}import kotlinx.serialization.Serializable\n\
         import kotlinx.serialization.json.Json\n\
         \n\
         @Serializable\n\
         data class Holder(val mode: Mode, val named: Named, val level: Level?, val modes: List<Mode>)\n\
         \n\
         fun box(): String {{\n\
         \x20   val holder = Holder(Mode.SLOW, Named.X, Level.HIGH, listOf(Mode.FAST, Mode.SLOW))\n\
         \x20   val text = Json.encodeToString(Holder.serializer(), holder)\n\
         \x20   return listOf(text, Json.decodeFromString(Holder.serializer(), text) == holder)\n\
         \x20       .joinToString(\" | \")\n\
         }}\n"
    )
}

const ROUND_TRIP: &str =
    "{\"mode\":\"SLOW\",\"named\":\"x\",\"level\":\"HIGH\",\"modes\":[\"FAST\",\"SLOW\"]} | true";

/// An enum of another file of the module.
#[test]
fn a_sibling_file_plain_enum_serializes_like_kotlinc() {
    let outcome = both_compilers_box_files(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Main.kt", &box_program("")),
        ],
        "sibling_plain_enum",
    );
    assert_eq!(outcome, ROUND_TRIP);
}

/// An enum of a dependency kotlinc built.
#[test]
fn a_dependency_plain_enum_serializes_like_kotlinc() {
    let outcome = both_compilers_box_against_dependency(
        DEPENDENCY,
        &box_program("import dep.*\n"),
        "dependency_plain_enum",
    );
    assert_eq!(outcome, ROUND_TRIP);
}

/// Each slot's factory is the call kotlinc builds, with the entry names the dependency states.
#[test]
fn a_dependency_plain_enum_element_is_built_the_way_kotlinc_builds_it() {
    let (plugin, runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let mut classpath = vec![reference_dependency(DEPENDENCY, "external_enum_bytes")];
    classpath.extend(runtime);
    let built =
        compare_with_kotlinc_plugin("Consumer", CONSUMER, "Holder", &classpath, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    for member in [
        "_childSerializers$_anonymous_()",
        "_childSerializers$_anonymous_$0()",
        "_childSerializers$_anonymous_$1()",
        "_childSerializers$_anonymous_$2()",
        "static {}",
    ] {
        let want = member_body(&built.reference, member);
        assert!(want.len() > 1, "Holder.{member}: the reference declares it");
        assert_eq!(member_body(&built.krusty, member), want, "Holder.{member}");
    }
}
