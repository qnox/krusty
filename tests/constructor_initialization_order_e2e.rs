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
open class Base(val value: Int)
fun box(): String {
    val one = 1
    class Local(n: Int) : Base(n + 1)
    val anon = object : Base(one + 1) {}
    return if (Local(one).value == 2 && anon.value == 2) "OK" else "fail"
}
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "AnonSuperForward"),
        Some("OK".to_string())
    );
}

#[test]
fn shadowed_super_argument_uses_the_resolved_binding() {
    let source = r#"
open class Base(val value: Int)
fun forwarded(value: Int): Int = value + 1
fun box(): String {
    val ok = 100
    class Holder(val ok: Int) {
        fun make(): Int {
            val anon = object : Base(ok) {}
            return anon.value
        }
    }
    fun nested(): Int {
        val ok = 10
        val stayed = object : Base(ok) {}
        val forwarded = object : Base(forwarded(ok)) {}
        val casted = object : Base(ok as Int) {}
        return stayed.value + forwarded.value + casted.value
    }
    val result = Holder(200).make() + nested() + ok
    return if (result == 231) "OK" else "fail"
}
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "ShadowedSuperArgument"),
        Some("OK".to_string())
    );
}

#[test]
fn object_super_call_evaluates_named_arguments_in_source_order() {
    let source = r#"
var order = 0
open class Base(val first: Int, val second: Int)
fun box(): String {
    val value = object : Base(
        second = { order = order * 10 + 1; 20 }(),
        first = { order = order * 10 + 2; 10 }(),
    ) {}
    return if (order == 12 && value.first == 10 && value.second == 20) "OK" else "fail"
}
"#;
    assert_eq!(common::expect_box_run_with_stdlib(source, "ArgOrder"), "OK");
}

#[test]
fn local_function_in_init_reads_the_constructor_property() {
    let source = r#"
var result1 = 0
var result2 = 0
class Test(val x: Int) {
    fun test(expected: Int) {
        result2 = if (result1 == expected) expected else -1
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
    val value = Test(7)
    value.test(7)
    return if (result2 == 7) "OK" else "fail"
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
class Payload(val value: Int)
class Element
@JvmInline
value class SingleInitBlock(val payload: Payload) {
    init {
        result = payload.value
    }
}
@JvmInline
value class MultipleInitBlocks(val element: Element?) {
    init {
        result = 3
    }
    init {
        result += 4
    }
}
@JvmInline
value class Getter(val payload: Payload) {
    init {
        result = current.value
    }
    val current: Payload
        get() = payload
}
@JvmInline
value class Method(val payload: Payload) {
    init {
        result = current(this).value
    }
    fun current(value: Method): Payload = value.payload
}
@JvmInline
value class InlineFun(val payload: Payload) {
    init {
        result = current().value
    }
    inline fun current(): Payload = payload
}
@JvmInline
value class Lambda(val payload: Payload) {
    init {
        val lambda = { result = payload.value }
        lambda()
    }
}
@JvmInline
value class LocalFunction(val payload: Payload) {
    init {
        fun local() {
            result = payload.value
        }
        local()
    }
}
@JvmInline
value class ObjectLiteral(val payload: Payload) {
    init {
        val objectLiteral = object {
            fun run() {
                result = payload.value
            }
        }
        objectLiteral.run()
    }
}
@JvmInline
value class LocalClass(val payload: Payload) {
    init {
        class Local {
            fun run() {
                result = payload.value
            }
        }
        Local().run()
    }
}
var result = 0
fun box(): String {
    val payload = Payload(7)
    SingleInitBlock(payload)
    if (result != 7) return "fail1"
    result = 0
    MultipleInitBlocks(null)
    if (result != 7) return "fail1b"
    result = 0
    Getter(payload)
    if (result != 7) return "fail1c"
    result = 0
    Method(payload)
    if (result != 7) return "fail1d"
    result = 0
    InlineFun(payload)
    if (result != 7) return "fail1e"
    result = 0
    Lambda(payload)
    if (result != 7) return "fail2"
    result = 0
    LocalFunction(payload)
    if (result != 7) return "fail3"
    result = 0
    ObjectLiteral(payload)
    if (result != 7) return "fail4"
    result = 0
    LocalClass(payload)
    return if (result == 7) "OK" else "fail5"
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
class Payload(val value: Int)
interface A {
    fun payload(): Payload
}
abstract class B(a: A) : A by a
"#;
    let main = r#"
class AImpl : A {
    override fun payload(): Payload = Payload(7)
}
class C : B(AImpl())
fun box(): String = if (C().payload().value == 7) "OK" else "fail"
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
open class Payload(val value: Int)
class Element(value: Int) : Payload(value)
@JvmInline value class LocalFunction<T : Payload>(val payload: T) {
    init {
        fun local() {
            result = payload
        }
        local()
    }
}
var result: Payload? = null
fun box(): String {
    val element = Element(7)
    LocalFunction(element)
    return if (result === element && result?.value == 7) "OK" else "fail"
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
open class Uniform(val accepted: Boolean) : Variable()
fun box(): String {
    var out: Uniform? = null
    val visitor = object : Program.Visitor<Unit>(Unit) {
        override fun visit(operand: Uniform): Unit {
            out = operand
        }
    }
    visitor.visit(Uniform(true))
    return if (out?.accepted == true) "OK" else "fail"
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
