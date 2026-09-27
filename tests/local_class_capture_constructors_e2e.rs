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

/// Compiles `source` with kotlinc and krusty and asserts each named class is byte-identical.
fn assert_identical(stem: &str, source: &str, classes: &[&str]) {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&source_path, source).expect("fixture source");
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let emitted = common::compile_in_process_metadata_cp(source, stem, &[common::stdlib_jar()])
        .expect("krusty compiles the fixture");
    let differing: Vec<&str> = classes
        .iter()
        .copied()
        .filter(|class| {
            let expected = std::fs::read(reference.join(format!("{class}.class")))
                .unwrap_or_else(|error| panic!("kotlinc did not emit {class}: {error}"));
            let (_, actual) = emitted
                .iter()
                .find(|(name, _)| name == class)
                .unwrap_or_else(|| panic!("krusty did not emit {class}"));
            *actual != expected
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(differing, Vec::<&str>::new(), "classes differ from kotlinc");
}

#[test]
fn captured_constructors_match_kotlinc() {
    assert_identical(
        "CaptureConstructors",
        SOURCE,
        &[
            "CaptureConstructorsKt$copied$1",
            "CaptureConstructorsKt$shared$1",
            "CaptureConstructorsKt$text$1",
            "CaptureConstructorsKt$withSuper$1",
            "CaptureConstructorsKt$localClass$L",
            "CaptureConstructorsKt$localSuper$L",
            "CaptureConstructorsKt$loop$1",
        ],
    );
}

/// A captured receiver is named after what it was: the enclosing instance is `this$0`, a named
/// extension callable's receiver `$this_<name>`, and an unlabeled lambda's `$this`. The constructor
/// parameter of the first two is the local `$receiver`; a lambda's is named like its field.
const RECEIVERS: &str = r##"fun interface Action {
    fun run()
}

class Box(val v: Int)

class Holder(val k: Int) {
    fun member(): Action = object : Action {
        override fun run() {
            k
        }
    }

    fun Box.memberExtension(): Action = object : Action {
        override fun run() {
            v
        }
    }

    fun local(): Any {
        class L {
            fun r() = k
        }
        return L()
    }
}

fun Box.extension(): Action = object : Action {
    override fun run() {
        v
    }
}

fun Int.primitive(): Action = object : Action {
    override fun run() {
        this@primitive
    }
}

fun Box.localClass(): Any {
    class L {
        fun r() = v
    }
    return L()
}

val property: Box.() -> Action = {
    object : Action {
        override fun run() {
            v
        }
    }
}

fun anonymous(): Any {
    val f = fun Box.(): Action = object : Action {
        override fun run() {
            v
        }
    }
    return f
}
"##;

#[test]
fn captured_receivers_are_named_like_kotlinc() {
    assert_identical(
        "CapturedReceivers",
        RECEIVERS,
        &[
            "Holder$member$1",
            "Holder$memberExtension$1",
            "Holder$local$L",
            "CapturedReceiversKt$extension$1",
            "CapturedReceiversKt$primitive$1",
            "CapturedReceiversKt$localClass$L",
            "CapturedReceiversKt$property$1$1",
            "CapturedReceiversKt$anonymous$f$1$1",
        ],
    );
}
