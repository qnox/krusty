//! A `@Serializable` class property whose type is an interface declared outside the file that is
//! not itself `@Serializable`: a JDK interface such as `java.nio.file.Path`.
//!
//! kotlinx.serialization serializes an interface polymorphically, so kotlinc's plugin builds
//! `PolymorphicSerializer(<interface>::class)` at the use site, in the property's
//! `$childSerializers` slot factory, wherever the element sits: directly, nullable, or as a type
//! argument of a generic or custom-serialized wrapper. krusty rejected the whole file ("not yet
//! supported by the IR backend") because only an interface declared in the same file had a plan.
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{
    compare_files_with_kotlinc_plugin, member_body, plugin_and_runtime,
};
use super::serialization_test_support::both_compilers_box_files;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   import java.nio.file.Path\n\
                   @Serializable\n\
                   class Direct(val path: Path)\n\
                   @Serializable\n\
                   class Wrapped(val paths: List<Path>, val maybe: Path?)\n";

fn compare(class: &str, members: &[&str]) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin("ExternalInterface", SRC, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    for member in members {
        let want = member_body(&built.reference, member);
        assert!(
            want.len() > 1,
            "kotlinc must emit {member} on {class}:\n{}",
            built.reference
        );
        assert_eq!(
            member_body(&built.krusty, member),
            want,
            "{class}.{member} differs from kotlinc"
        );
    }
}

/// A direct interface property: one cached slot whose factory builds the `PolymorphicSerializer`.
#[test]
fn an_external_interface_property_is_polymorphic_like_kotlinc() {
    compare("Direct", &["_childSerializers$_anonymous_()", "static {};"]);
}

/// A nullable interface (the slot holds the non-null serializer) and an interface as a `List`
/// element (an operand of the constructed list serializer).
#[test]
fn an_external_interface_nested_in_other_types_matches_kotlinc() {
    compare(
        "Wrapped",
        &[
            "_childSerializers$_anonymous_()",
            "_childSerializers$_anonymous_$0()",
            "static {};",
        ],
    );
}

/// Every member javap prints for a class, by its declaration line, in order.
fn declarations(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter(|line| {
            *line == "static {};"
                || (line.ends_with(';')
                    && line.contains('(')
                    && !line.contains(':')
                    && !line.starts_with('#'))
        })
        .map(str::to_string)
        .collect()
}

/// `class` compiled from `sources` by both compilers: the same members, each with kotlinc's body.
fn compare_every_member(stem: &str, sources: &[(&str, &str)], class: &str) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = match sources {
        [(_, source)] => compare_with_kotlinc_plugin(stem, source, class, &cp, "25", &extra),
        _ => compare_files_with_kotlinc_plugin(sources, class, &cp, &extra),
    }
    .expect("the reference compiler and javap must be available to this regression");
    let members = declarations(&built.reference);
    assert_eq!(declarations(&built.krusty), members, "{class}'s members");
    for member in &members {
        let name = member.split('(').next().unwrap_or(member);
        assert_eq!(
            member_body(&built.krusty, name),
            member_body(&built.reference, name),
            "{class}: {member} differs from kotlinc"
        );
    }
}

/// An interface declared in the same file: the same polymorphic element the external interface
/// has, `PolymorphicSerializer(<interface>::class, [])`, for a direct and a nullable property.
const SAME_FILE_INTERFACE: &str = "import kotlinx.serialization.Serializable\n\
                                   interface Animal\n\
                                   @Serializable\n\
                                   class Zoo(val a: Animal, val b: Animal?)\n";

#[test]
fn a_same_file_interface_property_is_polymorphic_like_kotlinc() {
    let sources = [("SameFileInterface.kt", SAME_FILE_INTERFACE)];
    compare_every_member("SameFileInterface", &sources, "Zoo");
    compare_every_member("SameFileInterface", &sources, "Zoo$$serializer");
}

/// A star projection of a class whose type parameter is bounded by an interface: the element of
/// `Box<*>` is `PolymorphicSerializer(E::class, [])`. kotlinc builds a `Box<*>` property's
/// serializer in `childSerializers` without a cache slot, but caches a `List<Box<*>>` like any
/// list.
const STAR_PROJECTION: &str = "import kotlinx.serialization.Serializable\n\
                               interface E\n\
                               @Serializable\n\
                               class Box<T : E>(val boxed: T)\n\
                               @Serializable\n\
                               class Wrapper(val boxed: Box<*>)\n\
                               @Serializable\n\
                               class Listed(val boxes: List<Box<*>>, val maybe: Box<*>?)\n";

#[test]
fn a_star_projected_element_is_polymorphic_like_kotlinc() {
    let sources = [("StarProjection.kt", STAR_PROJECTION)];
    for class in [
        "Wrapper",
        "Wrapper$$serializer",
        "Listed",
        "Listed$$serializer",
    ] {
        compare_every_member("StarProjection", &sources, class);
    }
}

/// An abstract `@Serializable` class: open polymorphism through the same element.
const ABSTRACT_CLASS: &str = "import kotlinx.serialization.Serializable\n\
                              @Serializable\n\
                              abstract class Base { abstract val v: Int }\n\
                              @Serializable\n\
                              class Holder(val b: Base, val c: Base?)\n";

#[test]
fn an_abstract_class_property_is_polymorphic_like_kotlinc() {
    let sources = [("AbstractClass.kt", ABSTRACT_CLASS)];
    compare_every_member("AbstractClass", &sources, "Holder");
    compare_every_member("AbstractClass", &sources, "Holder$$serializer");
}

/// An interface declared in a sibling file of the module, which is not `@Serializable`.
const SIBLING_INTERFACE: &str = "interface Animal\n\
                                 class Cat(val name: String) : Animal\n";
const SIBLING_ZOO: &str = "import kotlinx.serialization.Serializable\n\
                           @Serializable\n\
                           class Zoo(val a: Animal, val b: Animal?, val all: List<Animal>)\n";

#[test]
fn a_sibling_file_interface_property_is_polymorphic_like_kotlinc() {
    let sources = [("Animal.kt", SIBLING_INTERFACE), ("Zoo.kt", SIBLING_ZOO)];
    compare_every_member("SiblingInterface", &sources, "Zoo");
    compare_every_member("SiblingInterface", &sources, "Zoo$$serializer");
}

/// The runtime half on both compilers: the sibling interface's element descriptors are open
/// polymorphic, and a registered subclass encodes and decodes through them.
#[test]
fn a_sibling_file_interface_property_round_trips() {
    const MAIN: &str = "import kotlinx.serialization.*\n\
        import kotlinx.serialization.json.Json\n\
        import kotlinx.serialization.modules.*\n\
        @Serializable class SerialCat(val name: String) : Animal\n\
        fun box(): String {\n\
        \x20   val d = Zoo.serializer().descriptor\n\
        \x20   val kinds = (0 until d.elementsCount).joinToString { d.getElementDescriptor(it).serialName }\n\
        \x20   if (kinds != \"kotlinx.serialization.Polymorphic<Animal>, kotlinx.serialization.Polymorphic<Animal>?, kotlin.collections.ArrayList\") return \"FAIL descriptors: \" + kinds\n\
        \x20   val json = Json { serializersModule = SerializersModule { polymorphic(Animal::class) { subclass(SerialCat::class) } } }\n\
        \x20   val text = json.encodeToString(Zoo.serializer(), Zoo(SerialCat(\"a\"), null, listOf(SerialCat(\"b\"))))\n\
        \x20   val back = json.decodeFromString(Zoo.serializer(), text)\n\
        \x20   if ((back.a as SerialCat).name != \"a\" || back.b != null || (back.all.single() as SerialCat).name != \"b\") return \"FAIL round trip: \" + text\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(
        both_compilers_box_files(
            &[
                ("Animal.kt", SIBLING_INTERFACE),
                ("Zoo.kt", SIBLING_ZOO),
                ("Main.kt", MAIN)
            ],
            "sibling_interface_element"
        ),
        "OK"
    );
}
