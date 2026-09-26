//! The class a reference to a dependency's function compiles to, in the shape kotlinc's
//! `FunctionReferenceLowering` gives it: a `FunctionReferenceImpl` subclass with its own typed
//! `invoke` calling the declaration and the erased bridge to it, reflecting the declaration the way
//! kotlinc does. A member is owned by the class it is referenced on and reflects its JVM signature;
//! a top-level or extension function is owned by its file facade, flagged top-level, and its
//! signature includes an extension's receiver. A scalar parameter reaches the typed `invoke`
//! through `java/lang/Number`. Every carrier is compared byte for byte.

use super::common;

const LIBRARY: &str = r#"package lib

class Tank(val level: Int) {
    fun drain(): Int = level
    fun scale(by: Int): Int = level * by
}

fun Tank.spare(): Int = 2

fun Tank.mix(by: Int, mark: Char): String = "$mark"

fun twice(value: Int): Int = value * 2
"#;

const SOURCE: &str = r#"package app

import lib.Tank
import lib.mix
import lib.spare
import lib.twice

class Held(vararg val all: Any)

fun probe(tank: Tank): Held {
    val a = Tank::drain
    val b = tank::scale
    val c = Tank::spare
    val d = tank::mix
    val e = ::twice
    val f = Tank::scale
    return Held(a, b, c, d, e, f)
}
"#;

#[test]
fn library_function_reference_classes_are_byte_identical_to_kotlinc() {
    let dir = common::scratch_dir().expect("scratch directory");
    let library_out = dir.join("lib");
    let reference_out = dir.join("ref");
    std::fs::create_dir_all(&library_out).expect("library output directory");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let library = dir.join("Lib.kt");
    std::fs::write(&library, LIBRARY).expect("write library");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        library_out.to_string_lossy().into_owned(),
        library.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed on the library: {stderr}");
    let source = dir.join("Probe.kt");
    std::fs::write(&source, SOURCE).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-cp".to_string(),
        library_out.to_string_lossy().into_owned(),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let ours = common::compile_in_process_metadata_cp_module_target(
        SOURCE,
        "Probe",
        std::slice::from_ref(&library_out),
        "main",
        None,
    )
    .expect("krusty compiles the fixture");

    let carriers = [
        "app/ProbeKt$probe$a$1",
        "app/ProbeKt$probe$b$1",
        "app/ProbeKt$probe$c$1",
        "app/ProbeKt$probe$d$1",
        "app/ProbeKt$probe$e$1",
        "app/ProbeKt$probe$f$1",
    ];
    let mismatched = carriers
        .iter()
        .filter(|class| {
            let reference = std::fs::read(reference_out.join(format!("{class}.class")))
                .unwrap_or_else(|error| panic!("kotlinc did not emit {class}: {error}"));
            let (_, bytes) = ours
                .iter()
                .find(|(name, _)| name == *class)
                .unwrap_or_else(|| panic!("krusty did not emit {class}"));
            *bytes != reference
        })
        .collect::<Vec<_>>();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        mismatched,
        Vec::<&&str>::new(),
        "carriers that differ from kotlinc"
    );
}

const BOUNDARY_LIBRARY: &str = r#"package lib

@JvmInline
value class Box(val item: Any)

@JvmInline
value class Tag(val text: String)

@JvmName("labelOf")
fun Box.label(): String = "box:" + item

fun <T> T.describe(): String = "any:" + this

fun makeBox(item: Any): Box = Box(item)

fun <T> pass(value: T): T = value

fun twice(value: Int): Int = value * 2
"#;

const BOUNDARY_SOURCE: &str = r#"package app

import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext
import lib.Box
import lib.Tag
import lib.describe
import lib.label
import lib.makeBox
import lib.pass
import lib.twice

object Unused : Continuation<Int> {
    override val context: CoroutineContext get() = EmptyCoroutineContext
    override fun resumeWith(result: Result<Int>) {}
}

fun box(): String {
    val labelled = Box::label
    if (labelled(Box("a")) != "box:a") return "declared value-class receiver"
    val described = Box::describe
    if (described(Box("a")) != "any:Box(item=a)") return "generic receiver"
    val made = ::makeBox
    if (made("b").item != "b") return "declared value-class result"
    val passed: (Tag) -> Tag = ::pass
    if (passed(Tag("c")).text != "c") return "generic result specialized to a value class"
    val doubled: suspend (Int) -> Int = ::twice
    @Suppress("UNCHECKED_CAST")
    val raw = doubled as (Int, Continuation<Int>) -> Any?
    if (raw(21, Unused) != 42) return "suspend-converted reference"
    return "OK"
}
"#;

/// A dependency reference's adapter calls its declaration with the facts an ordinary call records.
/// `Box` erases to `Object` like a bare type parameter, so only the declared receiver and result
/// tell `labelOf(Box)` and `makeBox(): Box` (the carrier) from `T.describe()` and `pass(T): T` (a
/// box); the suspend-converted `::twice` returns its boxed result through the continuation-taking
/// `invoke`. These carriers keep the dispatching `invoke`, so they are checked at runtime.
#[test]
fn library_function_references_keep_declared_value_class_boundaries() {
    let dir = common::scratch_dir().expect("scratch directory");
    let library_out = dir.join("lib");
    std::fs::create_dir_all(&library_out).expect("library output directory");
    let library = dir.join("Lib.kt");
    std::fs::write(&library, BOUNDARY_LIBRARY).expect("write library");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        library_out.to_string_lossy().into_owned(),
        library.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed on the library: {stderr}");

    let ours = common::compile_in_process_metadata_cp_module_target(
        BOUNDARY_SOURCE,
        "Probe",
        &[library_out.clone(), common::stdlib_jar()],
        "main",
        None,
    )
    .expect("krusty compiles the fixture");
    let result = common::run_box(&ours, "app.ProbeKt", &[library_out, common::stdlib_jar()])
        .expect("box runner");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(result, "OK");
}
