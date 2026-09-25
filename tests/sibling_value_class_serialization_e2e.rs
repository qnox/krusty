//! A `@Serializable` class whose property is a value class declared in a SIBLING file.
//!
//! A constructor taking a value-class parameter is hidden: kotlinc makes it private and publishes a
//! synthetic accessor with a trailing `DefaultConstructorMarker`. The serialization plugin builds
//! the deserialization constructor `(int, …, SerializationConstructorMarker)` and must mark it the
//! same way — which it did for a value class in the same file and not for one in another file:
//!
//! ```text
//! kotlinc:  private synthetic Holder(int, Tag, SerializationConstructorMarker)
//!           public  synthetic Holder(int, Tag, SerializationConstructorMarker, DefaultConstructorMarker)
//! krusty:   public  synthetic Holder(int, Tag, SerializationConstructorMarker)
//! ```
//!
//! The plugin sees only the file it runs on. Its common-IR value-class table had not been populated
//! since the AST lowerer was removed, so a sibling-file value class read as an ordinary class. The
//! plugin runner now publishes the classifier provider's checked facts into that same table before
//! generation; the JVM representation pass consumes and extends the single authority later.
//!
//! Generated clients put each value class in a file of its own and use it from data classes in
//! other files, so this is the ordinary shape there rather than an edge case.
use super::common::method_instructions;
use super::serialization_companion_byte_parity_e2e::{
    compare_files_with_kotlinc_plugin, plugin_and_runtime,
};
use super::serialization_test_support::both_compilers_box_files;

const VALUE: &str = "import kotlinx.serialization.Serializable\n\
    @JvmInline\n\
    @Serializable\n\
    value class RawTag(val value: Int)\n\
    @JvmInline\n\
    @Serializable\n\
    value class Tag(val value: RawTag)\n";

const HOLDER: &str = "import kotlinx.serialization.Serializable\n\
    @Serializable\n\
    class Holder(val count: Tag)\n";

/// `Holder`'s constructors as `(access flags, descriptor)`, sorted — the constructor SET. Member
/// order is compared elsewhere; a separate difference there must not mask this one.
fn constructors(bytes: &[u8]) -> Vec<(u16, String)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("a parseable class");
    let mut out = class
        .methods
        .iter()
        .filter(|method| method.name == "<init>")
        .map(|method| (method.access, method.descriptor.clone()))
        .collect::<Vec<_>>();
    out.sort();
    out
}

#[test]
fn a_sibling_value_class_hides_the_deserialization_constructor() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_files_with_kotlinc_plugin(
        &[("Tag.kt", VALUE), ("Holder.kt", HOLDER)],
        "Holder",
        &cp,
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    const PRIVATE_SYNTHETIC: u16 = 0x1002;
    const PUBLIC_SYNTHETIC: u16 = 0x1001;
    let serialization = "(ILTag;Lkotlinx/serialization/internal/SerializationConstructorMarker;)V";
    let accessor = "(ILTag;Lkotlinx/serialization/internal/SerializationConstructorMarker;\
                    Lkotlin/jvm/internal/DefaultConstructorMarker;)V";
    let reference = constructors(&built.reference_bytes);
    assert!(
        reference.contains(&(PRIVATE_SYNTHETIC, serialization.to_string()))
            && reference.contains(&(PUBLIC_SYNTHETIC, accessor.to_string())),
        "the reference must hide the serialization constructor behind an accessor: {reference:?}"
    );
    assert_eq!(constructors(&built.krusty_bytes), reference);
    let marker = "SerializationConstructorMarker)";
    assert_eq!(
        method_instructions(&built.krusty, marker),
        method_instructions(&built.reference, marker),
        "complete deserialization-constructor instructions"
    );
}

/// The runtime half: JSON decoding reaches the hidden constructor through its accessor, including
/// a transitive sibling value-class carrier (`Tag` → `RawTag` → `Int`).
#[test]
fn a_sibling_value_class_round_trips() {
    const MAIN: &str = "import kotlinx.serialization.json.Json\n\
        fun box(): String {\n\
        \x20   val back = Json.decodeFromString(Holder.serializer(), \"{\\\"count\\\":5}\")\n\
        \x20   if (back.count.value.value != 5) return \"FAIL decode: \" + back.count\n\
        \x20   val text = Json.encodeToString(Holder.serializer(), back)\n\
        \x20   if (text != \"{\\\"count\\\":5}\") return \"FAIL encode: \" + text\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(
        both_compilers_box_files(
            &[("Tag.kt", VALUE), ("Holder.kt", HOLDER), ("Main.kt", MAIN)],
            "sibling_value_class"
        ),
        "OK"
    );
}
