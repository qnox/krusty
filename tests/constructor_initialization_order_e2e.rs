//! Constructor properties are parameters until they are stored, then properties.
//! Anonymous-object super-constructor arguments that are not constants or lexical captures are
//! evaluated at the construction site and forwarded. Every class below is compared byte for byte
//! with kotlinc.
use super::common;

const SOURCE: &str = r##"open class Base(val a: Int)

open class Pair(val a: Int, val b: Int = 0)

open class Measured(val a: Int, val b: Float = 0f, val c: Boolean = false)

interface I {
    fun v(): Int
}

fun mk(n: Int): I = object : I {
    override fun v() = n
}

fun side(): Int = 1

class Later(private val x: Int) {
    val y = x
    init {
        val z = x
    }
}

class Child(private val x: Int) : Base(x) {
    val y = x
    init {
        val z = x
        class Local {
            fun read() = x
        }
        val local = Local()
    }
    fun method() = x
}

class Plain(n: Int) {
    val next = n + 1
    init {
        val z = n
    }
}

class ByProp(private val x: Int) : I by mk(x)

class Both(private val x: Int) : Base(x), I by mk(x)

class PropDefault(private val x: Int, val y: Int = x)

class ParamDefault(x: Int, val y: Int = x)

class BodyOnly(private val cacheLimit: Int) {
    val map = object : Base(1) {
        fun sizeOk() = cacheLimit
    }
}

class TwoProps(private val cacheLimit: Int) {
    private val loadFactor = 0.75f
    private val initialCapacity = cacheLimit + 1
    val map = object : Measured(initialCapacity, loadFactor, true) {
        fun sizeOk() = cacheLimit
    }
}

class CallArg {
    val map = object : Base(side()) {}
}

class ExprArg(val n: Int) {
    val map = object : Base(n + 1) {}
}

class ConstFold {
    val map = object : Base(1 + 2) {}
}

class CaptureAndExpr {
    fun make(n: Int, m: Int): Pair {
        return object : Pair(n, m + 1) {
            fun read() = n
        }
    }
}

class InInit(private val x: Int) {
    init {
        val o = object {
            fun v() = x
        }
    }
}
"##;

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
fn anonymous_and_local_super_arguments_keep_their_values() {
    let source = r#"
open class Base(val value: String)
fun box(): String {
    val ok = "O"
    class Local(n: String) : Base(n + "K")
    val anon = object : Base(ok + "K") {}
    return Local(ok).value + anon.value
}
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "AnonSuperForward"),
        Some("OKOK".to_string())
    );
}

#[test]
fn shadowed_super_argument_uses_the_resolved_binding() {
    let source = r#"
open class Base(val value: String)
fun box(): String {
    val ok = "OUTER"
    class Holder(val ok: String) {
        fun make(): String {
            val anon = object : Base(ok) {}
            return anon.value
        }
    }
    fun nested(): String {
        val ok = "INNER"
        val stayed = object : Base(ok) {}
        val forwarded = object : Base(ok + "!") {}
        val casted = object : Base(ok as String) {}
        return stayed.value + forwarded.value + casted.value
    }
    return Holder("PARAM").make() + nested() + ok
}
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "ShadowedSuperArgument"),
        Some("OUTERINNERINNER!INNEROUTER".to_string())
    );
}

#[test]
fn constructor_properties_and_anonymous_super_arguments_match_kotlinc() {
    assert_identical(
        "CtorInit",
        SOURCE,
        &[
            "Later",
            "Child",
            "Child$Local",
            "Plain",
            "ByProp",
            "Both",
            "PropDefault",
            "ParamDefault",
            "BodyOnly",
            "BodyOnly$map$1",
            "TwoProps",
            "TwoProps$map$1",
            "CallArg",
            "CallArg$map$1",
            "ExprArg",
            "ExprArg$map$1",
            "ConstFold",
            "ConstFold$map$1",
            "CaptureAndExpr$make$1",
            "InInit",
            "InInit$o$1",
        ],
    );
}
