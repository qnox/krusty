//! The class a property reference compiles to, in the shape kotlinc's `FunctionReferenceLowering`
//! gives it: a synthetic final `(Mutable)PropertyReferenceNImpl` subclass enclosed by the function
//! the reference is written in, carrying the `k=3` synthetic-class metadata, whose `get`/`set`
//! overrides are attributed to the reference's line and name their locals as kotlinc does.
//!
//! Each carrier shape is covered: an unbound member (`Tank::level`, arity 1), a bound member
//! (`tank::level`, arity 0 with a stored receiver), and a top-level property (`::depth`, arity 0
//! singleton), each for a `val` and a `var`; a scalar `var`'s `set` unboxes its erased value
//! through `java/lang/Number`. Every carrier is compared byte for byte.

use super::common;

const SOURCE: &str = r#"package app

class Tank(val level: Int) {
    var label: String = ""
    var count: Int = 0
}

val depth: Int = 3
var mark: String = ""
var total: Int = 0

class Held(vararg val all: Any)

fun probe(tank: Tank): Held {
    val a = Tank::level
    val b = Tank::label
    val c = tank::level
    val d = tank::label
    val e = ::depth
    val f = ::mark
    val g = Tank::count
    val h = tank::count
    val i = ::total
    return Held(a, b, c, d, e, f, g, h, i)
}
"#;

#[test]
fn property_reference_classes_are_byte_identical_to_kotlinc() {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference_out = dir.join("ref");
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let source = dir.join("Probe.kt");
    std::fs::write(&source, SOURCE).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let ours =
        common::compile_in_process_metadata_cp_module_target(SOURCE, "Probe", &[], "main", None)
            .expect("krusty compiles the fixture");

    let carriers = [
        "app/ProbeKt$probe$a$1",
        "app/ProbeKt$probe$b$1",
        "app/ProbeKt$probe$c$1",
        "app/ProbeKt$probe$d$1",
        "app/ProbeKt$probe$e$1",
        "app/ProbeKt$probe$f$1",
        "app/ProbeKt$probe$g$1",
        "app/ProbeKt$probe$h$1",
        "app/ProbeKt$probe$i$1",
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
