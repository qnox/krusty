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

use std::path::PathBuf;
use std::sync::OnceLock;

use super::common;

/// Collect every `<prefix>*.jar` under a root (no `-sources`).
/// The kotlinx.serialization runtime, PROVISIONED rather than discovered.
///
/// Crawling `~/.gradle` makes the test depend on whatever a developer happens to have cached: it
/// panics on a machine that has never resolved the artifact (CI), and on a machine that has several
/// it silently picks one. `ensure_maven` fetches the pinned version into the shared dependency cache
/// — the same path the coroutines runtime uses — so every run compiles against the same bytes.
const SERIALIZATION_VERSION: &str = "1.9.0";

fn provisioned(artifact: &str) -> PathBuf {
    krusty::toolchain::ensure_maven("org.jetbrains.kotlinx", artifact, SERIALIZATION_VERSION)
        .unwrap_or_else(|| {
            panic!(
                "could not provision {artifact}:{SERIALIZATION_VERSION}; this test must not \
                 self-skip, since a skipped serialization test passes on a compiler that would \
                 have rejected the fixture. Check network access or set KRUSTY_DEPS_CACHE."
            )
        })
}

fn runtime_jars() -> Vec<PathBuf> {
    static JARS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    JARS.get_or_init(|| {
        vec![
            common::stdlib_jar(),
            provisioned("kotlinx-serialization-core-jvm"),
            provisioned("kotlinx-serialization-json-jvm"),
        ]
    })
    .clone()
}

/// Compile in krusty and RUN the result. A compile-only assertion would not catch a serializer that
/// is wired to the wrong runtime class, which is the failure mode this table entry can produce.
/// Compile and run the SAME fixture with the reference compiler, on the SAME runtime jars and its
/// own serialization plugin.
///
/// Running only krusty proves krusty agrees with itself. These fixtures assert exact JSON, so the
/// question they exist to answer — does krusty pick the serializer kotlinc picks — is only answered
/// by running kotlinc over the identical source and comparing the same value.
fn reference_box(src: &str, stem: &str) -> String {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-reference"));
    std::fs::create_dir_all(&work).expect("create reference fixture directory");
    let source = work.join("Main.kt");
    std::fs::write(&source, src).expect("write reference fixture");
    let out = work.join("classes");
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{stem}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "{stem}: the reference serialization plugin is missing at {}",
        plugin.display()
    );
    let jars = runtime_jars();
    let joined = std::env::join_paths(&jars).expect("join the reference classpath");
    let (code, diagnostics) = common::kotlinc_compile(&[
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-opt-in=kotlin.time.ExperimentalTime,kotlin.uuid.ExperimentalUuidApi".to_string(),
        "-cp".to_string(),
        joined.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .unwrap_or_else(|| panic!("{stem}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{stem}: kotlinc rejected the fixture: {diagnostics}"
    );
    let mut cp = vec![out];
    cp.extend(jars);
    common::run_box(&[], "MainKt", &cp)
        .unwrap_or_else(|| panic!("{stem}: the reference-built box() did not run"))
}

/// Run one fixture under BOTH compilers and require the same `box()` value.
fn both_compilers_box(src: &str, stem: &str) -> String {
    let reference = reference_box(src, stem);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    run_box(src, stem)
}

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
    assert_eq!(both_compilers_box(MAIN, "instant_property"), "OK");
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
    assert_eq!(both_compilers_box(MAIN, "instant_element"), "OK");
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
    assert_eq!(both_compilers_box(MAIN, "uuid_builtin"), "OK");
}
