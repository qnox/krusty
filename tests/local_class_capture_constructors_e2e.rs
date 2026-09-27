//! kotlinc's `LocalDeclarationsLowering` gives a local class or anonymous object a constructor that
//! takes each captured value as a synthetic leading parameter named like its field (`$a`), stores it
//! before delegating to the super constructor, guards none of them with `checkNotNullParameter`,
//! starts the line table after those stores, and lists none of them in the class's metadata
//! constructor. Every class below is compared byte for byte with kotlinc's.
use super::common;

const SOURCE: &str = r##"fun interface Action {
    fun run()
}

open class Base(val x: Int)

fun copied(a: Int): Action = object : Action {
    override fun run() {
        a
    }
}

fun shared(): Action {
    var c = 0
    return object : Action {
        override fun run() {
            c = c + 1
        }
    }
}

fun text(s: String): Action = object : Action {
    override fun run() {
        s
    }
}

fun withSuper(a: Int): Base = object : Base(a) {
    fun r() = a
}

fun localClass(a: Int): Any {
    class L(val z: Int) {
        fun r() = a + z
    }
    return L(1)
}

fun localSuper(a: Int): Any {
    class L(z: Int) : Base(z) {
        fun r() = a
    }
    return L(1)
}

fun loop(): Action? {
    var r: Action? = null
    for (i in 0..1) {
        r = object : Action {
            override fun run() {
                i
            }
        }
    }
    return r
}
"##;

const STEM: &str = "CaptureConstructors";

#[test]
fn captured_constructors_match_kotlinc() {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source_path = dir.join(format!("{STEM}.kt"));
    std::fs::write(&source_path, SOURCE).expect("fixture source");
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp(SOURCE, STEM, &[common::stdlib_jar()])
        .expect("krusty compiles the fixture");
    let differing: Vec<&str> = [
        "CaptureConstructorsKt$copied$1",
        "CaptureConstructorsKt$shared$1",
        "CaptureConstructorsKt$text$1",
        "CaptureConstructorsKt$withSuper$1",
        "CaptureConstructorsKt$localClass$L",
        "CaptureConstructorsKt$localSuper$L",
        "CaptureConstructorsKt$loop$1",
    ]
    .into_iter()
    .filter(|class| {
        let expected = std::fs::read(reference.join(format!("{class}.class")))
            .unwrap_or_else(|error| panic!("kotlinc did not emit {class}: {error}"));
        let (_, actual) = classes
            .iter()
            .find(|(name, _)| name == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        *actual != expected
    })
    .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(differing, Vec::<&str>::new(), "classes differ from kotlinc");
}
