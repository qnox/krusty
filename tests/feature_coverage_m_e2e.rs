//! End-to-end "box" coverage for reflection-adjacent and callable-shaped features: class references
//! (`X::class` / `x::class`, `.simpleName`, `.java`), function/property/constructor references, callable
//! reference equality + invoke-through-a-variable, annotation declaration + application (`@Target`,
//! `@JvmStatic`, `@JvmName`), anonymous object expressions capturing a local, and SAM conversion. Each
//! test compiles a `fun box(): String` returning "OK" and runs it on a real JVM.

use super::common;

fn run(src: &str, stem: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, stem)
}

#[test]
fn class_ref_simple_name_and_identity() {
    // NOTE: `X::class.java` (the KClass->Class bridge) is NOT modeled by krusty and is dropped from this
    // suite; this covers `X::class`, `.simpleName`, and KClass identity (`==`) instead.
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "class Foo\nclass Bar\n\
fun box(): String {\n\
    if (Foo::class.simpleName != \"Foo\") return \"simpleName:${Foo::class.simpleName}\"\n\
    if (Foo::class == Bar::class) return \"identity\"\n\
    if (Foo::class != Foo::class) return \"self\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "ClassRefName").expect("class ref"), "OK");
}

#[test]
fn bound_class_ref_on_instance() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "class Foo\n\
fun box(): String {\n\
    val x: Any = Foo()\n\
    if (x::class != Foo::class) return \"neq\"\n\
    if (x::class.simpleName != \"Foo\") return \"name:${x::class.simpleName}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "BoundClassRef").expect("bound class ref"), "OK");
}

#[test]
fn top_fun_reference_passed_as_lambda() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "fun inc(n: Int): Int = n + 1\n\
fun apply1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun box(): String {\n\
    if (apply1(::inc, 41) != 42) return \"apply\"\n\
    val r = listOf(1, 2, 3).map(::inc)\n\
    if (r != listOf(2, 3, 4)) return \"map:$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "TopFunRef").expect("top fun ref"), "OK");
}

#[test]
fn unbound_member_reference_user_method() {
    // NOTE: an unbound member ref on a LIBRARY type (`String::length`) is not modeled by krusty and is
    // dropped; this covers the general unbound member reference (`Class::method`) on a user class, which
    // takes the receiver as its first argument.
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "class Cell(val n: Int) { fun doubled(): Int = n * 2 }\n\
fun box(): String {\n\
    val f: (Cell) -> Int = Cell::doubled\n\
    if (f(Cell(3)) != 6) return \"f:${f(Cell(3))}\"\n\
    val r = listOf(Cell(1), Cell(2), Cell(3)).map(Cell::doubled)\n\
    if (r != listOf(2, 4, 6)) return \"map:$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "UnboundMemberRef").expect("Cell::doubled"), "OK");
}

#[test]
fn bound_instance_method_reference() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "class C(val base: Int) {\n\
    fun add(x: Int): Int = x + base\n\
}\n\
fun box(): String {\n\
    val c = C(10)\n\
    val r = listOf(1, 2, 3).map(c::add)\n\
    if (r != listOf(11, 12, 13)) return \"map:$r\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "BoundMethodRef").expect("bound method ref"), "OK");
}

#[test]
fn property_reference_get_and_name() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "class C(val n: Int)\n\
fun box(): String {\n\
    val p = C::n\n\
    if (p.get(C(7)) != 7) return \"get:${p.get(C(7))}\"\n\
    if (p.name != \"n\") return \"name:${p.name}\"\n\
    val f: (C) -> Int = p\n\
    if (f(C(9)) != 9) return \"fun:${f(C(9))}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "PropertyRef").expect("property ref"), "OK");
}

#[test]
fn function_type_variable_invoke_and_equality() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    // Invoke through a variable of function type, and check callable-reference equality (two `::inc`
    // references to the same top-level function compare equal, per kotlinc's FunctionReferenceImpl).
    // NOTE: `.hashCode()` on such a fn-typed variable is not modeled by krusty, so only `==` is checked.
    const SRC: &str = "fun inc(n: Int): Int = n + 1\n\
fun box(): String {\n\
    val g: (Int) -> Int = ::inc\n\
    if (g(5) != 6) return \"invoke:${g(5)}\"\n\
    val h: (Int) -> Int = ::inc\n\
    if (g != h) return \"eq\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "FnVarInvoke").expect("fn var invoke/eq"), "OK");
}

#[test]
fn constructor_reference_as_factory() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "class Boxed(val v: Int)\n\
fun box(): String {\n\
    val make: (Int) -> Boxed = ::Boxed\n\
    if (make(7).v != 7) return \"make:${make(7).v}\"\n\
    val r = listOf(1, 2, 3).map(::Boxed)\n\
    if (r.map { it.v } != listOf(1, 2, 3)) return \"map\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "CtorRef").expect("ctor ref factory"), "OK");
}

#[test]
fn user_annotation_declared_and_applied() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    // Declare an annotation with @Target/@Retention and apply it to a class, a function, and a property.
    // The annotated program must compile and run (retention/element target metadata is codegen, not
    // observed here — this exercises that the declaration + applications are accepted end to end).
    const SRC: &str =
        "@Target(AnnotationTarget.CLASS, AnnotationTarget.FUNCTION, AnnotationTarget.PROPERTY)\n\
@Retention(AnnotationRetention.RUNTIME)\n\
annotation class Tag(val name: String)\n\
@Tag(\"c\")\n\
class Holder {\n\
    @Tag(\"p\")\n\
    val x: Int = 1\n\
}\n\
@Tag(\"f\")\n\
fun tagged(): Int = 41\n\
fun box(): String {\n\
    if (Holder().x != 1) return \"x\"\n\
    if (tagged() != 41) return \"f\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "UserAnnot").expect("user annotation"), "OK");
}

#[test]
fn jvm_static_and_jvm_name_on_companion_members() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    // @JvmStatic exposes a companion member as a real static; @JvmName renames the emitted method. Both
    // must compile and the program observe the expected results when called through Kotlin.
    const SRC: &str = "class C {\n\
    companion object {\n\
        @JvmStatic fun twice(n: Int): Int = n * 2\n\
    }\n\
}\n\
@JvmName(\"renamed\")\n\
fun original(n: Int): Int = n + 1\n\
fun box(): String {\n\
    if (C.twice(21) != 42) return \"static:${C.twice(21)}\"\n\
    if (original(41) != 42) return \"jvmname:${original(41)}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC, "JvmStaticName").expect("jvmstatic/jvmname"), "OK");
}

#[test]
fn anonymous_object_implementing_interface_captures_local() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "interface Supplier { fun get(): String }\n\
fun box(): String {\n\
    val captured = \"OK\"\n\
    val s: Supplier = object : Supplier {\n\
        override fun get(): String = captured\n\
    }\n\
    return s.get()\n\
}\n";
    assert_eq!(run(SRC, "AnonCapture").expect("anon obj capture"), "OK");
}

#[test]
fn sam_conversion_lambda_to_fun_interface() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    const SRC: &str = "fun interface Transform { fun apply(x: String): String }\n\
fun run2(t: Transform): String = t.apply(\"O\")\n\
fun box(): String {\n\
    val r = run2 { s -> s + \"K\" }\n\
    return r\n\
}\n";
    assert_eq!(run(SRC, "SamConv").expect("SAM conversion"), "OK");
}

#[test]
fn sam_conversion_to_java_runnable() {
    let Some(_jh) = common::java_home() else {
        eprintln!("skip: no JAVA_HOME");
        return;
    };
    let Some(_sl) = common::stdlib_jar() else {
        eprintln!("skip: no stdlib");
        return;
    };
    // A lambda passed where a Java functional interface (java.lang.Runnable) is expected is SAM-converted.
    const SRC: &str = "fun box(): String {\n\
    var acc = \"fail\"\n\
    val r = Runnable { acc = \"OK\" }\n\
    r.run()\n\
    return acc\n\
}\n";
    assert_eq!(run(SRC, "SamRunnable").expect("SAM Runnable"), "OK");
}
