//! `kotlin.time.Instant` has a built-in element serializer.
//!
//! ```kotlin
//! @Serializable
//! data class Stamped(val name: String, val at: Instant)
//! ```
//!
//! krusty refused the file: `krusty: this construct is not yet supported by the IR backend`. The
//! builtin table maps the primitives, `String` and `kotlin.uuid.Uuid`, but not `kotlin.time.Instant`,
//! so no element serializer could be derived and the plugin left its `serialize-body` placeholder —
//! which fails the whole FILE.
//!
//! The runtime ships `kotlinx/serialization/internal/InstantSerializer`, exactly parallel to the
//! `UuidSerializer` entry already in the table.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::common;

/// Collect every `<prefix>*.jar` under a root (no `-sources`).
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

/// The numeric version embedded in a jar file name, for ordering. `kotlinx-serialization-core-jvm-
/// 1.11.0.jar` sorts above `…-1.6.3.jar`.
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

/// The NEWEST matching jar. Picking the first match found instead selects whichever version happens
/// to be encountered first, which silently resolves to a runtime that predates the serializer under
/// test — the control here failed exactly that way with `NoClassDefFoundError: UuidSerializer`.
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

/// Compile in krusty and RUN the result. A compile-only assertion would not catch a serializer that
/// is wired to the wrong runtime class, which is the failure mode this table entry can produce.
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

/// The failing shape: an `Instant`-typed property of a `@Serializable` class.
#[test]
fn an_instant_property_serializes_through_the_builtin() {
    const MAIN: &str = "import kotlin.time.ExperimentalTime\n\
import kotlin.time.Instant\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@OptIn(ExperimentalTime::class)\n\
@Serializable\n\
data class Stamped(val name: String, val at: Instant)\n\
\n\
@OptIn(ExperimentalTime::class)\n\
fun box(): String {\n\
\x20   val value = Stamped(\"x\", Instant.fromEpochSeconds(0))\n\
\x20   val json = Json.encodeToString(Stamped.serializer(), value)\n\
\x20   if (json != \"{\\\"name\\\":\\\"x\\\",\\\"at\\\":\\\"1970-01-01T00:00:00Z\\\"}\") return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Stamped.serializer(), json)\n\
\x20   return if (back == value) \"OK\" else \"FAIL: round trip \" + back\n\
}\n";
    assert_eq!(run_box(MAIN, "instant_property"), "OK");
}

/// The same type reached through a collection ELEMENT, which is the path the builtin table serves.
#[test]
fn an_instant_collection_element_serializes_through_the_builtin() {
    const MAIN: &str = "import kotlin.time.ExperimentalTime\n\
import kotlin.time.Instant\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@OptIn(ExperimentalTime::class)\n\
@Serializable\n\
data class Timeline(val stamps: List<Instant>)\n\
\n\
@OptIn(ExperimentalTime::class)\n\
fun box(): String {\n\
\x20   val value = Timeline(listOf(Instant.fromEpochSeconds(0)))\n\
\x20   val json = Json.encodeToString(Timeline.serializer(), value)\n\
\x20   if (json != \"{\\\"stamps\\\":[\\\"1970-01-01T00:00:00Z\\\"]}\") return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Timeline.serializer(), json)\n\
\x20   return if (back == value) \"OK\" else \"FAIL: round trip \" + back\n\
}\n";
    assert_eq!(run_box(MAIN, "instant_element"), "OK");
}

/// The control: `Uuid`, the entry this one is modelled on, still works.
#[test]
fn the_uuid_builtin_still_works() {
    const MAIN: &str = "import kotlin.uuid.ExperimentalUuidApi\n\
import kotlin.uuid.Uuid\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@OptIn(ExperimentalUuidApi::class)\n\
@Serializable\n\
data class Keyed(val id: Uuid)\n\
\n\
@OptIn(ExperimentalUuidApi::class)\n\
fun box(): String {\n\
\x20   val value = Keyed(Uuid.parse(\"00000000-0000-0000-0000-000000000000\"))\n\
\x20   val json = Json.encodeToString(Keyed.serializer(), value)\n\
\x20   return if (json == \"{\\\"id\\\":\\\"00000000-0000-0000-0000-000000000000\\\"}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(run_box(MAIN, "uuid_builtin"), "OK");
}
