//! A `@kotlinx.serialization.Transient` property is not a serial element.
//!
//! kotlinc's serialization plugin leaves it out of the descriptor, `childSerializers`, `write$Self`
//! and `deserialize`, and the deserialization constructor takes no argument for it: it runs the
//! property's initializer, which a transient property must have (the plugin's frontend rejects one
//! without, `serialization_transient_diagnostics_e2e`). krusty serialized it as an ordinary element
//! (`{"x":1,"cache":7}` where kotlinc writes `{"x":1}`).
//!
//! Once a transient property precedes another one, element `i` is no longer backing field `i`: the
//! element index, its seen-mask bit and its constructor argument slot all shift, while the field
//! written or read stays the property's own.
//!
//! A property is transient by the resolved identity of its annotation, never by its spelling: a
//! `typealias` or an import alias of `kotlinx.serialization.Transient` is transient, and an unrelated
//! annotation class that is also called `Transient` is not.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{
    compare_files_with_kotlinc_plugin, plugin_and_runtime,
};
use super::serialization_test_support::{both_compilers_box, both_compilers_box_files};

/// The shapes a transient property takes: a constructor property between two elements, a body
/// property whose initializer reads an earlier property, and a property of a generic class. Both
/// compilers must write and read the same documents.
#[test]
fn transient_properties_are_neither_written_nor_read() {
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

@Serializable
data class Ledger(val id: Int, @Transient val scratch: Int = 5, val label: String = "none")

@Serializable
class Tally(val count: Int) {
    @Transient
    var memo: String = "m$count"
    val doubled: Int = count * 2
}

@Serializable
data class Crate<T>(val item: T, @Transient val note: String = "n", val size: Int)

fun box(): String {
    val ledger = Json.encodeToString(Ledger.serializer(), Ledger(1, 9, "x"))
    val ledgerBack = Json.decodeFromString(Ledger.serializer(), """{"id":2,"label":"y"}""")
    val tally = Json.encodeToString(Tally.serializer(), Tally(3).also { it.memo = "changed" })
    val tallyBack = Json.decodeFromString(Tally.serializer(), """{"count":4}""")
    val crates = Crate.serializer(Int.serializer())
    val crate = Json.encodeToString(crates, Crate(7, "z", 2))
    val crateBack = Json.decodeFromString(crates, """{"item":8,"size":3}""")
    return "$ledger|$ledgerBack|$tally|${tallyBack.memo}/${tallyBack.doubled}|$crate|$crateBack"
}
"#;
    let outcome = both_compilers_box(src, "transient_properties");
    assert_eq!(
        outcome,
        "{\"id\":1,\"label\":\"x\"}|Ledger(id=2, scratch=5, label=y)|{\"count\":3}|m4/8|\
         {\"item\":7,\"size\":2}|Crate(item=8, note=n, size=3)"
    );
}

/// The reported shape: `Json.encodeToString(P(1, 7))` writes only `x`, and decoding restores the
/// transient property from its initializer.
#[test]
fn a_transient_constructor_property_is_left_out_of_the_document() {
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient
import kotlinx.serialization.json.Json
import kotlinx.serialization.encodeToString

@Serializable
data class P(val x: Int, @Transient val cache: Int = 0)

fun box(): String =
    Json.encodeToString(P(1, 7)) + " " + Json.decodeFromString<P>("""{"x":1}""")
"#;
    assert_eq!(
        both_compilers_box(src, "transient_reported_shape"),
        "{\"x\":1} P(x=1, cache=0)"
    );
}

/// Every class the plugin touches is byte-identical to kotlinc's when a transient property sits
/// between two elements: the descriptor's element count and names, `childSerializers`,
/// `deserialize`, `write$Self`, and the deserialization constructor's parameters, seen-mask bits
/// and the initializer it runs for the transient property.
#[test]
fn a_class_with_a_transient_property_is_byte_identical() {
    let (plugin, cp) =
        plugin_and_runtime().expect("the serialization plugin and runtime are provisioned");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.Transient\n\
               @Serializable\n\
               data class Reading(val sensor: Int, @Transient val cached: Int = 3, val level: Int)\n";
    for class in ["Reading", "Reading$$serializer", "Reading$Companion"] {
        let built = compare_with_kotlinc_plugin("TransientBytes", src, class, &cp, "25", &extra)
            .expect("reference kotlinc and javap are provisioned");
        assert_byte_identical(class, &built);
    }
}

fn assert_byte_identical(class: &str, built: &super::common::ReferenceComparison) {
    if built.krusty_bytes != built.reference_bytes {
        let (want, got) = (disassembly(&built.reference), disassembly(&built.krusty));
        let first = want
            .iter()
            .zip(&got)
            .position(|(want, got)| want != got)
            .unwrap_or_else(|| want.len().min(got.len()));
        panic!(
            "{class} differs from kotlinc; first difference at line {first}\n  kotlinc: {:?}\n  krusty:  {:?}",
            want.get(first),
            got.get(first),
        );
    }
}

/// `javap -v` output without the lines that name the file rather than describe the class.
fn disassembly(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| {
            let line = line.trim_start();
            ![
                "Classfile ",
                "Last modified",
                "SHA-256",
                "MD5",
                "Compiled from",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .collect()
}

/// A transient body property runs its initializer in the deserialization constructor at its place
/// in declaration order, between the elements around it, on its own line.
#[test]
fn a_transient_body_property_is_initialized_by_the_deserialization_constructor() {
    let (plugin, cp) =
        plugin_and_runtime().expect("the serialization plugin and runtime are provisioned");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.Transient\n\
               @Serializable\n\
               class Meter(val start: Int) {\n\
               \x20   @Transient\n\
               \x20   val offset: Int = 4\n\
               \x20   val end: Int = 9\n\
               }\n";
    let built =
        compare_with_kotlinc_plugin("TransientBodyProperty", src, "Meter", &cp, "25", &extra)
            .expect("reference kotlinc and javap are provisioned");
    let signature =
        "Meter(int, int, int, kotlinx.serialization.internal.SerializationConstructorMarker);";
    let want = constructor_code(&built.reference, signature);
    assert!(
        want.iter().any(|line| line.contains("iconst_4")),
        "kotlinc runs the transient initializer: {want:?}"
    );
    assert_eq!(
        constructor_code(&built.krusty, signature),
        want,
        "Meter's deserialization constructor"
    );
    let serializer = compare_with_kotlinc_plugin(
        "TransientBodyProperty",
        src,
        "Meter$$serializer",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are provisioned");
    assert!(
        serializer.krusty_bytes == serializer.reference_bytes,
        "Meter$$serializer differs from kotlinc"
    );
}

/// The instructions and line table of the constructor `signature` in a `javap -c -l -v` listing,
/// without constant-pool indices: those differ with unrelated pool order, the instructions do not.
fn constructor_code(disassembly: &str, signature: &str) -> Vec<String> {
    let start = disassembly
        .find(signature)
        .unwrap_or_else(|| panic!("no constructor {signature} in:\n{disassembly}"));
    let body = &disassembly[start..];
    let end = body
        .find("LocalVariableTable")
        .expect("the constructor has debug tables");
    body[..end]
        .lines()
        .map(|line| {
            line.split_once(" #")
                .map_or(line, |(instruction, _)| instruction)
                .trim_end()
                .to_string()
        })
        .collect()
}

/// A `typealias` and an import alias of `kotlinx.serialization.Transient` name the annotation itself,
/// so their properties are transient; an unrelated annotation class declared as `Transient` is just
/// another annotation, and its property stays an element. Both compilers write and read the same
/// documents.
#[test]
fn an_aliased_transient_is_transient_and_a_same_named_annotation_is_not() {
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient as Skipped
import kotlinx.serialization.json.Json

typealias Skip = kotlinx.serialization.Transient

@Target(AnnotationTarget.PROPERTY)
annotation class Transient

@Serializable
data class Aliased(
    val id: Int,
    @Skip val viaTypealias: Int = 1,
    @Transient val unrelated: Int = 2,
    @Skipped val viaImport: Int = 3,
)

@Serializable
class Body(val id: Int) {
    @Skip
    var cached: String = "c$id"
    @Transient
    var kept: String = "k"
}

fun box(): String {
    val aliased = Json.encodeToString(Aliased.serializer(), Aliased(5, 6, 7, 8))
    val aliasedBack = Json.decodeFromString(
        Aliased.serializer(),
        """{"id":9,"unrelated":4}""",
    )
    val body = Json.encodeToString(Body.serializer(), Body(1).also { it.cached = "x"; it.kept = "y" })
    val bodyBack = Json.decodeFromString(Body.serializer(), """{"id":2,"kept":"z"}""")
    return "$aliased|$aliasedBack|$body|${bodyBack.cached}/${bodyBack.kept}"
}
"#;
    assert_eq!(
        both_compilers_box(src, "transient_aliases"),
        "{\"id\":5,\"unrelated\":7}|Aliased(id=9, viaTypealias=1, unrelated=4, viaImport=3)|\
         {\"id\":1,\"kept\":\"y\"}|c2/z"
    );
}

/// The alias shapes above are byte-identical to kotlinc: which properties are elements decides the
/// descriptor, the serializer's members and the deserialization constructor's parameters. (Every
/// element is required here, which keeps `write$Self` free of default comparisons.)
#[test]
fn a_class_with_aliased_and_same_named_transients_is_byte_identical() {
    let (plugin, cp) =
        plugin_and_runtime().expect("the serialization plugin and runtime are provisioned");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.Transient as Skipped\n\
               typealias Skip = kotlinx.serialization.Transient\n\
               @Target(AnnotationTarget.PROPERTY)\n\
               annotation class Transient\n\
               @Serializable\n\
               data class Probe(val id: Int, @Skip val viaTypealias: Int = 1, \
               @Transient val unrelated: Int, @Skipped val viaImport: Int = 3)\n";
    for class in ["Probe", "Probe$$serializer", "Probe$Companion"] {
        let built =
            compare_with_kotlinc_plugin("TransientAliasBytes", src, class, &cp, "25", &extra)
                .expect("reference kotlinc and javap are provisioned");
        assert_byte_identical(class, &built);
    }
}

/// A `lateinit` transient property has no initializer and needs none: kotlinc accepts it, and its
/// deserialization constructor leaves the field unset. The runtime result and that constructor
/// match kotlinc.
#[test]
fn a_lateinit_transient_property_is_left_unset_by_deserialization() {
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient
import kotlinx.serialization.json.Json

@Serializable
class Session(val user: String) {
    @Transient
    lateinit var token: String
}

fun box(): String {
    val written = Json.encodeToString(Session.serializer(), Session("a").also { it.token = "t" })
    val read = Json.decodeFromString(Session.serializer(), """{"user":"b"}""")
    val unset = runCatching { read.token }.isFailure
    return "$written|${read.user}|$unset"
}
"#;
    assert_eq!(
        both_compilers_box(src, "transient_lateinit"),
        "{\"user\":\"a\"}|b|true"
    );

    let (plugin, cp) =
        plugin_and_runtime().expect("the serialization plugin and runtime are provisioned");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let declaration = "import kotlinx.serialization.Serializable\n\
                       import kotlinx.serialization.Transient\n\
                       @Serializable\n\
                       class Session(val user: String) {\n\
                       \x20   @Transient\n\
                       \x20   lateinit var token: String\n\
                       }\n";
    let built = compare_with_kotlinc_plugin(
        "TransientLateinit",
        declaration,
        "Session",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are provisioned");
    let signature = "Session(int, java.lang.String, \
                     kotlinx.serialization.internal.SerializationConstructorMarker);";
    assert_eq!(
        constructor_code(&built.krusty, signature),
        constructor_code(&built.reference, signature),
        "Session's deserialization constructor"
    );
    let serializer = compare_with_kotlinc_plugin(
        "TransientLateinit",
        declaration,
        "Session$$serializer",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_byte_identical("Session$$serializer", &serializer);
}

/// The serializable class and its transient properties are declared in one file, and serialized from
/// another. The streaming frontend releases the declaring file's syntax before the source set is
/// lowered, so which properties are transient must travel with the checked properties rather than be
/// recovered from source. Both compilers write and read the same documents, and the declaring
/// file's classes are byte-identical to kotlinc's.
#[test]
fn a_transient_property_declared_in_another_file_stays_transient() {
    const MODEL: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.Transient\n\
\n\
typealias Skip = kotlinx.serialization.Transient\n\
\n\
@Serializable\n\
data class Order(val id: Int, @Skip val draft: Boolean = true, val total: Int)\n\
\n\
@Serializable\n\
class Note(val text: String) {\n\
\x20   @Transient\n\
\x20   val label: String = \"note\"\n\
}\n";
    const MAIN: &str = "import kotlinx.serialization.json.Json\n\
\n\
fun box(): String {\n\
\x20   val order = Json.encodeToString(Order.serializer(), Order(1, false, 30))\n\
\x20   val orderBack = Json.decodeFromString(Order.serializer(), \"\"\"{\"id\":2,\"total\":5}\"\"\")\n\
\x20   val note = Json.encodeToString(Note.serializer(), Note(\"t\"))\n\
\x20   val noteBack = Json.decodeFromString(Note.serializer(), \"\"\"{\"text\":\"u\"}\"\"\")\n\
\x20   return \"$order|$orderBack|$note|${noteBack.text}/${noteBack.label}\"\n\
}\n";
    assert_eq!(
        both_compilers_box_files(&[("Model.kt", MODEL), ("Main.kt", MAIN)], "transient_files"),
        "{\"id\":1,\"total\":30}|Order(id=2, draft=true, total=5)|{\"text\":\"t\"}|u/note"
    );

    // The byte comparison links against the serialization core only, so its user file reaches
    // the serializers without the JSON format. It comes first, so the declaring file is not the
    // first one the frontend processes.
    const USE: &str = "fun elements(): Int =\n\
\x20   Order.serializer().descriptor.elementsCount + Note.serializer().descriptor.elementsCount\n";
    let (plugin, cp) =
        plugin_and_runtime().expect("the serialization plugin and runtime are provisioned");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    for class in ["Order", "Order$$serializer", "Note", "Note$$serializer"] {
        let built = compare_files_with_kotlinc_plugin(
            &[("Use.kt", USE), ("Model.kt", MODEL)],
            class,
            &cp,
            &extra,
        )
        .expect("reference kotlinc and javap are provisioned");
        assert_byte_identical(class, &built);
    }
}
