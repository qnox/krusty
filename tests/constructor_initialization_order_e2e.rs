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
fn object_super_call_evaluates_named_arguments_in_source_order() {
    let source = r#"
var result = "fail"
open class Base(val o: String, val k: String)
fun box(): String {
    val obj1 = object : Base(k = { result = "O"; "K" }(), o = { result += "K"; "O" }()) {}
    if (result != "OK") return "fail $result"
    return obj1.o + obj1.k
}
"#;
    assert_eq!(common::expect_box_run_with_stdlib(source, "ArgOrder"), "OK");
}

#[test]
fn local_function_in_init_reads_the_constructor_property() {
    let source = r#"
lateinit var result1: String
lateinit var result2: String
class Test(val x: String) {
    fun test(a: String) {
        if (result1 != a) throw AssertionError("result1: $result1")
        result2 = a
    }
    init {
        fun test() {
            fun test1() {
                result1 = x
            }
            test1()
        }
        test()
    }
}
fun box(): String {
    val t = Test("OK")
    t.test("OK")
    return result2
}
"#;
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "LocalFunInit"),
        "OK"
    );
}

#[test]
fn value_class_init_reads_its_property() {
    let source = r#"
@JvmInline
value class SingleInitBlock(val s: String) {
    init {
        res = s
    }
}
@JvmInline
value class MultipleInitBlocks(val a: Any?) {
    init {
        res = "O"
    }
    init {
        res += "K"
    }
}
@JvmInline
value class Getter(val s: String) {
    init {
        res = ok
    }
    val ok: String
        get() = s
}
@JvmInline
value class Method(val s: String) {
    init {
        res = ok(this)
    }
    fun ok(m: Method): String = m.s
}
@JvmInline
value class InlineFun(val s: String) {
    init {
        res = ok()
    }
    inline fun ok(): String = s
}
@JvmInline
value class Lambda(val s: String) {
    init {
        val lambda = { res = s }
        lambda()
    }
}
@JvmInline
value class LocalFunction(val s: String) {
    init {
        fun local() {
            res = s
        }
        local()
    }
}
@JvmInline
value class ObjectLiteral(val s: String) {
    init {
        val objectLiteral = object {
            fun run() {
                res = s
            }
        }
        objectLiteral.run()
    }
}
@JvmInline
value class LocalClass(val s: String) {
    init {
        class Local {
            fun run() {
                res = s
            }
        }
        Local().run()
    }
}
var res: String = "FAIL"
fun box(): String {
    SingleInitBlock("OK")
    if (res != "OK") return "fail1 $res"
    res = "FAIL"
    MultipleInitBlocks(null)
    if (res != "OK") return "fail1b $res"
    res = "FAIL"
    Getter("OK")
    if (res != "OK") return "fail1c $res"
    res = "FAIL"
    Method("OK")
    if (res != "OK") return "fail1d $res"
    res = "FAIL"
    InlineFun("OK")
    if (res != "OK") return "fail1e $res"
    res = "FAIL"
    Lambda("OK")
    if (res != "OK") return "fail2 $res"
    res = "FAIL"
    LocalFunction("OK")
    if (res != "OK") return "fail3 $res"
    res = "FAIL"
    ObjectLiteral("OK")
    if (res != "OK") return "fail4 $res"
    res = "FAIL"
    LocalClass("OK")
    return res
}
"#;
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "ValueInit"),
        "OK"
    );
}

#[test]
fn interface_delegation_across_modules_forwards_the_member() {
    let lib = r#"
interface A {
    fun foo(): String
}
abstract class B(a: A) : A by a
"#;
    let main = r#"
class AImpl : A {
    override fun foo(): String = "OK"
}
class C : B(AImpl())
fun box(): String = C().foo()
"#;
    let dir = common::scratch_dir().expect("scratch directory");
    let emitted =
        common::compile_in_process_metadata_cp(lib, "DelegationLib", &[common::stdlib_jar()])
            .expect("the dependency module compiles");
    let jar_path = dir.join("delegation-lib.jar");
    let file = std::fs::File::create(&jar_path).expect("create the dependency jar");
    let mut archive = zip::ZipWriter::new(file);
    for (name, bytes) in &emitted {
        archive
            .start_file(
                format!("{name}.class"),
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .expect("start a class entry");
        use std::io::Write;
        archive.write_all(bytes).expect("write a class entry");
    }
    archive.finish().expect("finish the dependency jar");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let result = common::expect_box_run(
        main,
        "DelegationMain",
        &[stdlib, jar_path],
        Some(jdk.as_path()),
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(result, "OK");
}

#[test]
fn generic_value_class_init_reads_its_property() {
    let source = r#"
// LANGUAGE: +GenericInlineClassParameter
@JvmInline
value class LocalFunction<T : String>(val s: T) {
    init {
        fun local() {
            res = s
        }
        local()
    }
}
var res: String = "FAIL"
fun box(): String {
    LocalFunction("OK")
    return res
}
"#;
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "GenericValueInit"),
        "OK"
    );
}

#[test]
fn anonymous_object_forwards_a_unit_super_argument() {
    let source = r#"
open class Operand
open class Variable : Operand()
class Program {
    class Stm(val o: Operand)
    open class Visitor<E>(val default: E) {
        open fun visit(o: Operand): E {
            if (o is Uniform) return visit(o)
            return default
        }
        open fun visit(operand: Uniform): E = default
    }
}
open class Uniform(val result: String) : Variable()
fun box(): String {
    val out = ArrayList<Uniform>()
    val visitor = object : Program.Visitor<Unit>(Unit) {
        override fun visit(operand: Uniform): Unit {
            out.add(operand)
        }
    }
    visitor.visit(Uniform("OK"))
    return out[0].result
}
"#;
    assert_eq!(common::expect_box_run_with_stdlib(source, "AnonUnit"), "OK");
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
