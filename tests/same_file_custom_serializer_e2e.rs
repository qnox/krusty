//! A class-level `@Serializable(with = …)` declared in the SAME file is honored.
//!
//! ```kotlin
//! @Serializable(with = FlexSerializer::class)
//! class Flex(val raw: String)
//!
//! @Serializable
//! data class Holder(val payload: Flex)
//! ```
//!
//! krusty refused the file outright:
//!
//! ```text
//! error: krusty: this construct is not yet supported by the IR backend
//! [lower] JVM emission declined residual IR nodes:
//!   [(…, PluginPlaceholder { plugin: "serialization", kind: "serialize-body", … })]
//! ```
//!
//! The serialization plugin leaves that placeholder rather than emit a half-built serializer, and a
//! residual node fails the whole FILE.
//!
//! The same class declared in a SIBLING file always worked — that asymmetry is the discriminator, and
//! it is the reverse of what one would guess: the sibling case resolves through the external-serializer
//! map, while a same-file class is expected to derive its own `$serializer` and its class-level
//! `with =` was not consulted.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::common;

fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 10 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, prefix, depth + 1, out);
        } else if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            if name.starts_with(prefix) && name.ends_with(".jar") && !name.contains("sources") {
                out.push(path.clone());
            }
        }
    }
}

/// The numeric version embedded in a jar file name, for ordering. Sorting the PATHS as strings
/// instead puts `1.9` above `1.10`; comparing the parsed components orders them properly.
fn version_key(path: &Path) -> Vec<u64> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| {
            stem.rsplit('-')
                .next()
                .unwrap_or_default()
                .split('.')
                .map(|part| part.parse::<u64>().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default()
}

/// The NEWEST matching jar. Taking the FIRST match encountered instead selects whichever version the
/// directory walk happens to reach first, which can silently resolve to a runtime that predates the
/// serializer under test — that failure mode produced a `NoClassDefFoundError` in a sibling suite,
/// which reads as a compiler bug rather than a test-fixture bug.
fn find(prefix: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set to locate the serialization runtime");
    let mut found = Vec::new();
    walk(&Path::new(&home).join(".gradle"), prefix, 0, &mut found);
    found.sort_by_key(|path| version_key(path));
    found.pop().unwrap_or_else(|| {
        panic!(
            "no {prefix}*.jar under ~/.gradle, so this test cannot run.\n\
             It must not self-skip: a skipped serialization test passes on an unfixed compiler."
        )
    })
}

fn runtime_jars() -> Vec<PathBuf> {
    static JARS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    JARS.get_or_init(|| {
        vec![
            common::stdlib_jar(),
            find("kotlinx-serialization-core-jvm"),
            find("kotlinx-serialization-json-jvm"),
        ]
    })
    .clone()
}

/// Compile the program entirely in krusty and run its `box()`. Panics loudly on a compile failure —
/// this defect IS a compile failure, so a skip here would report success on an unfixed compiler.
fn run_box(src: &str, stem: &str) -> String {
    let jars = runtime_jars();
    let classes = common::compile_in_process(src, stem, &jars, None).unwrap_or_else(|| {
        let outcome = common::backend_outcome_in_process(src, stem, &jars, None);
        panic!("krusty failed to compile {stem}: {outcome:?}")
    });
    let box_class =
        common::find_box_class(&classes).unwrap_or_else(|| panic!("no box class for {stem}"));
    common::run_box(&classes, &box_class, &jars)
        .unwrap_or_else(|| panic!("box() did not run for {stem}"))
}

const SERIALIZER: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable(with = FlexSerializer::class)\n\
class Flex(val raw: String)\n\
\n\
object FlexSerializer : KSerializer<Flex> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"Flex\", PrimitiveKind.STRING)\n\
\n\
\x20   override fun serialize(encoder: Encoder, value: Flex) {\n\
\x20       encoder.encodeString(value.raw)\n\
\x20   }\n\
\n\
\x20   override fun deserialize(decoder: Decoder): Flex = Flex(decoder.decodeString())\n\
}\n\
\n";

/// The failing shape: the annotated class is a DIRECT property type.
#[test]
fn a_same_file_custom_serializer_serves_a_direct_property() {
    let main = format!(
        "{SERIALIZER}\
@Serializable\n\
data class Holder(val payload: Flex)\n\
fun box(): String {{\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Flex(\"deep\")))\n\
\x20   return if (json == \"{{\\\"payload\\\":\\\"deep\\\"}}\") \"OK\" else \"FAIL: \" + json\n\
}}\n"
    );
    assert_eq!(run_box(&main, "same_file_direct"), "OK");
}

/// The corpus shape: the annotated class reached through a collection ELEMENT.
#[test]
fn a_same_file_custom_serializer_serves_a_map_value() {
    let main = format!(
        "{SERIALIZER}\
@Serializable\n\
data class Holder(val fields: Map<String, Flex>)\n\
fun box(): String {{\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(mapOf(\"k\" to Flex(\"deep\"))))\n\
\x20   return if (json == \"{{\\\"fields\\\":{{\\\"k\\\":\\\"deep\\\"}}}}\") \"OK\" else \"FAIL: \" + json\n\
}}\n"
    );
    assert_eq!(run_box(&main, "same_file_map_value"), "OK");
}

/// The control that isolates the same-file case: a LIST element behaves like the map value.
#[test]
fn a_same_file_custom_serializer_serves_a_list_element() {
    let main = format!(
        "{SERIALIZER}\
@Serializable\n\
data class Holder(val fields: List<Flex>)\n\
fun box(): String {{\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(listOf(Flex(\"deep\"))))\n\
\x20   return if (json == \"{{\\\"fields\\\":[\\\"deep\\\"]}}\") \"OK\" else \"FAIL: \" + json\n\
}}\n"
    );
    assert_eq!(run_box(&main, "same_file_list_element"), "OK");
}

/// The control that shows nothing about ordinary derivation changed: a `@Serializable` class with no
/// custom serializer still derives its own.
#[test]
fn an_ordinary_serializable_property_still_derives() {
    const MAIN: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Inner(val raw: String)\n\
\n\
@Serializable\n\
data class Holder(val payload: Inner)\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Inner(\"deep\")))\n\
\x20   return if (json == \"{\\\"payload\\\":{\\\"raw\\\":\\\"deep\\\"}}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(run_box(MAIN, "ordinary_derivation"), "OK");
}

/// A custom serializer declared as a CLASS rather than an `object` takes constructor arguments
/// (`ValueSerializer<T>(dataSerializer)`), so it has no `INSTANCE` field. Reading one would emit a
/// reference to a field that does not exist, which is worse than declining: this shape must stay a
/// clean refusal until a plan that CONSTRUCTS the serializer exists. Asserted as a refusal, not as a
/// success, so the day it starts working this test says so.
#[test]
fn a_class_valued_custom_serializer_is_still_declined_rather_than_miscompiled() {
    const MAIN: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
\n\
@Serializable(with = BoxSerializer::class)\n\
class Box<T>(val item: T)\n\
\n\
class BoxSerializer<T>(private val itemSerializer: KSerializer<T>) : KSerializer<Box<T>> {\n\
\x20   override val descriptor: SerialDescriptor = itemSerializer.descriptor\n\
\n\
\x20   override fun serialize(encoder: Encoder, value: Box<T>) {\n\
\x20       itemSerializer.serialize(encoder, value.item)\n\
\x20   }\n\
\n\
\x20   override fun deserialize(decoder: Decoder): Box<T> = Box(itemSerializer.deserialize(decoder))\n\
}\n\
\n\
@Serializable\n\
data class Holder(val payload: Box<String>)\n";
    let jars = runtime_jars();
    let outcome = common::backend_outcome_in_process(MAIN, "class_valued_serializer", &jars, None);
    let rendered = format!("{outcome:?}");
    assert!(
        rendered.contains("not yet supported"),
        "a class-valued custom serializer must be declined cleanly, not miscompiled into a \
         non-existent INSTANCE read; got: {rendered}"
    );
}
