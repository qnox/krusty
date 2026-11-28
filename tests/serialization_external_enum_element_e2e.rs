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
use super::compiler_plugin_fixture::PluginFixture;
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

const SERIAL_INFO_DEPENDENCY: &[(&str, &str)] = &[(
    "Tagged.kt",
    "package info\n\
     import kotlinx.serialization.SerialInfo\n\
     @OptIn(kotlinx.serialization.ExperimentalSerializationApi::class)\n\
     @SerialInfo\n\
     @Target(AnnotationTarget.CLASS, AnnotationTarget.PROPERTY)\n\
     annotation class Tag(val t: String)\n\
     enum class EntryTagged { @Tag(\"a\") A, B }\n\
     @Tag(\"e\") enum class ClassTagged { A, B }\n",
)];

/// kotlinc hands a dependency enum's `@SerialInfo` annotations, on the enum or an entry, to the
/// runtime factory as instances of a generated implementation class, which the published facts do
/// not describe. Each such element declines rather than build a descriptor without them.
#[test]
fn a_dependency_plain_enum_with_a_serial_info_annotation_declines() {
    for (name, holder) in [
        (
            "entry-tagged",
            "import kotlinx.serialization.Serializable\n\
             @Serializable\n\
             class Holder(val e: info.EntryTagged)\n",
        ),
        (
            "class-tagged",
            "import kotlinx.serialization.Serializable\n\
             @Serializable\n\
             class Holder(val e: info.ClassTagged)\n",
        ),
    ] {
        let (plugin, runtime) = plugin_and_runtime()
            .expect("the serialization plugin and runtime must be available to this regression");
        let switches = vec![format!("-Xplugin={}", plugin.display())];
        let fixture = PluginFixture::new(&format!("external-enum-serial-info-{name}"));
        let reference_lib = fixture.kotlinc("lib", SERIAL_INFO_DEPENDENCY, &runtime, &switches);
        let sources = [("Main.kt", holder)];
        let mut classpath = runtime.clone();
        classpath.push(reference_lib);
        fixture.kotlinc("main", &sources, &classpath, &switches);
        assert_eq!(
            fixture.krusty_rejects("main", &sources, &classpath, &switches),
            "<fixture>/main/src/Main.kt:1:1: error: krusty: this construct is not yet supported by the IR backend\n\
             import kotlinx.serialization.Serializable\n\
             ^\n\
             krusty: 1 error(s)\n",
            "{name}"
        );
    }
}
