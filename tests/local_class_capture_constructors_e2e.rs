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

/// Local and anonymous classes capturing anonymous context parameters (`context(_: Box)`), which
/// kotlinc stores in a field named after the parameter's own label (`$$context-Box`). The implicit
/// context argument read from that field has no source position, so each `run` marks its line only
/// at the call.
const CONTEXT_RECEIVERS: &str = r##"
interface Action {
    fun run(): Any
}

class Box(val v: Any)

class Holder(val t: Any)

class Two(val first: Any, val second: Any)

context(b: Box)
fun readBox(): Any = b.v

context(h: Holder)
fun readHolder(): Any = h.t

context(_: Box)
fun single(): Action = object : Action {
    override fun run() = readBox()
}

context(_: Box, _: Holder)
fun both(): Action = object : Action {
    override fun run() = Two(readHolder(), readBox())
}

context(_: Box, _: Holder)
fun second(): Action = object : Action {
    override fun run() = readHolder()
}

context(_: Box)
fun localClass(): Any {
    class L {
        fun r() = readBox()
    }
    return L()
}
"##;

#[test]
fn captured_context_receivers_are_named_like_kotlinc() {
    assert_identical(
        "ContextReceivers",
        CONTEXT_RECEIVERS,
        &[
            "ContextReceiversKt$single$1",
            "ContextReceiversKt$both$1",
            "ContextReceiversKt$second$1",
            "ContextReceiversKt$localClass$L",
        ],
    );
}

const FUNCTION_TYPE_CONTEXTS: &str = r##"
interface Action {
    fun run(): Any
}

class Crate(val v: Any)

class Shelf(val w: Any)

class Two(val first: Any, val second: Any)

context(c: Crate)
fun readCrate(): Any = c.v

context(s: Shelf)
fun readShelf(): Any = s.w

fun one(): context(Crate) () -> Action = {
    object : Action {
        override fun run() = readCrate()
    }
}

fun two(): context(Crate, Shelf) () -> Action = {
    object : Action {
        override fun run() = Two(readShelf(), readCrate())
    }
}

fun withReceiver(): context(Crate) Shelf.() -> Action = {
    object : Action {
        override fun run() = Two(w, readCrate())
    }
}
"##;

/// A lambda checked against a function type with context parameters captures them as kotlinc
/// names anonymous context parameters (`$$context-Crate`), whether the last one stands as the
/// lambda's current receiver or an extension receiver takes that place.
#[test]
fn captured_function_type_context_receivers_are_named_like_kotlinc() {
    assert_identical(
        "FunctionTypeContexts",
        FUNCTION_TYPE_CONTEXTS,
        &[
            "FunctionTypeContextsKt$one$1$1",
            "FunctionTypeContextsKt$two$1$1",
            "FunctionTypeContextsKt$withReceiver$1$1",
        ],
    );
}

/// kotlinc 2.4.20 no longer compiles legacy `context(Box)` receivers, so there are no reference
/// bytes: this pins the distinct legacy capture role, which spells its field after the legacy
/// parameter name (`$context_receiver_0`) rather than an anonymous parameter's label.
#[test]
fn a_captured_legacy_context_receiver_keeps_its_own_role() {
    let source = "// LANGUAGE: +ContextReceivers\n\
         interface Action {\n\
         \x20   fun run(): Any\n\
         }\n\
         \n\
         class Box(val v: Any)\n\
         \n\
         context(Box)\n\
         fun legacy(): Action = object : Action {\n\
         \x20   override fun run() = v\n\
         }\n";
    let emitted = common::compile_in_process_metadata_cp(source, "Legacy", &[common::stdlib_jar()])
        .expect("krusty compiles the fixture");
    let (_, bytes) = emitted
        .iter()
        .find(|(name, _)| name == "LegacyKt$legacy$1")
        .expect("krusty emits the anonymous object");
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse the anonymous object");
    let fields = class
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.descriptor.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(fields, [("$$context_receiver_0", "LBox;")]);
}
