//! Property references into a compiled dependency bind the exact accessor or field the
//! dependency's metadata records, never one found again by owner and spelling.
//!
//! Every declaration here has a same-spelled rival: `level`'s getter is renamed to `fetchLevel`
//! beside an unrelated `getLevel()`, `note`'s accessors are renamed beside `getNote()`/`setNote()`,
//! and the inherited `@JvmField stamp` sits beside a `getStamp()` method. Each carrier is compared
//! byte for byte with kotlinc, and `box()` observes at runtime which member each reference reached.

use super::common;

const LIBRARY: &str = r#"package lib

open class Base {
    @JvmField var stamp: String = "field"
    fun getStamp(): String = "method"
}

open class Tank : Base() {
    @get:JvmName("fetchLevel")
    val level: Int = 5
    fun getLevel(): Int = 99
    var note: String = "note"
        @JvmName("readNote") get
        @JvmName("writeNote") set
    var shadow: String = "unset"
    fun getNote(): String = "method"
    fun setNote(value: String) { shadow = value }
}

class Sub : Tank()
"#;

const SOURCE: &str = r#"package app

import lib.Sub
import lib.Tank

class Held(vararg val all: Any)

fun probe(tank: Tank, sub: Sub): Held {
    val a = Tank::level
    val b = sub::level
    val c = Tank::note
    val d = sub::note
    val e = Sub::stamp
    val f = tank::stamp
    return Held(a, b, c, d, e, f)
}

fun box(): String {
    val sub = Sub()
    val level = Sub::level
    val note = sub::note
    note.set("set")
    val stamp = Sub::stamp
    stamp.set(sub, "stamped")
    val bound = sub::stamp
    if (level.get(sub) != 5) return "level reached getLevel()"
    if (note.get() != "set") return "note read reached getNote()"
    if (sub.shadow != "unset") return "note write reached setNote()"
    if (sub.stamp != "stamped") return "stamp write missed the field"
    if (bound.get() != "stamped") return "stamp read reached getStamp()"
    return "OK"
}
"#;

#[test]
fn dependency_property_references_bind_the_recorded_accessors() {
    let dir = common::scratch_dir().expect("scratch directory");
    let library_out = dir.join("lib");
    let reference_out = dir.join("ref");
    std::fs::create_dir_all(&library_out).expect("library output directory");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let library = dir.join("Lib.kt");
    let source = dir.join("Probe.kt");
    std::fs::write(&library, LIBRARY).expect("write library");
    std::fs::write(&source, SOURCE).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        library_out.to_string_lossy().into_owned(),
        library.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed on the library: {stderr}");
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
        &[library_out.clone(), common::stdlib_jar()],
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
        "app/ProbeKt$box$level$1",
        "app/ProbeKt$box$note$1",
        "app/ProbeKt$box$stamp$1",
        "app/ProbeKt$box$bound$1",
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
    assert_eq!(
        mismatched,
        Vec::<&&str>::new(),
        "carriers differ from kotlinc"
    );

    let result = common::run_box(
        &ours,
        "app.ProbeKt",
        &[library_out.clone(), common::stdlib_jar()],
    )
    .expect("box runner");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(result, "OK");
}
