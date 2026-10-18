//! A local class declared inside a function body (`class L { … }`) is hoisted to a
//! top-level-equivalent class and lowered like any other class. Slice 1: non-capturing local classes
//! (a capturing one — e.g. a super-constructor argument referencing an outer local — stays unsupported
//! and the file skips, never miscompiles). Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn local_class_basic() {
    const SRC: &str = "fun box(): String {\n\
    class Pair(val a: Int, val b: Int) {\n\
        fun sum() = a + b\n\
    }\n\
    val p = Pair(1, 2)\n\
    if (p.sum() != 3) return \"fail sum\"\n\
    if (p.a != 1 || p.b != 2) return \"fail fields\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("local class should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn local_data_class() {
    // A local `data class` uses component/equals via the data-class machinery.
    const SRC: &str = "fun box(): String {\n\
    data class Point(val x: Int, val y: Int)\n\
    val p = Point(3, 4)\n\
    if (p.x + p.y != 7) return \"fail data\"\n\
    if (p != Point(3, 4)) return \"fail eq\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "LocalDataClass");
}

#[test]
fn named_local_interface_is_rejected() {
    const SRC: &str = r#"fun box(): String {
    interface Greeter { fun hi(): String }
    return "OK"
}
"#;
    let result = common::compiler_diagnostics(&[("Main.kt", SRC)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let expected = [common::CompilerError {
        file: "Main.kt".to_string(),
        line: 2,
        column: 5,
        message: "interface 'Greeter' cannot be local. Try to use an anonymous object or abstract class instead.".to_string(),
    }];
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), expected);
}

#[test]
fn local_class_inheritance_with_modifiers() {
    // Modifier-prefixed local classes (`open`/`abstract`) and local-class inheritance — none capturing
    // an outer local (slice 2a).
    const SRC: &str = "fun box(): String {\n\
    open class Base { open fun name() = \"base\" }\n\
    class Derived : Base() { override fun name() = \"derived\" }\n\
    if (Base().name() != \"base\") return \"fail base\"\n\
    if (Derived().name() != \"derived\") return \"fail derived\"\n\
    val b: Base = Derived()\n\
    if (b.name() != \"derived\") return \"fail virtual\"\n\
    abstract class Shape { abstract fun area(): Int }\n\
    class Sq(val s: Int) : Shape() { override fun area() = s * s }\n\
    if (Sq(3).area() != 9) return \"fail abstract\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("local class inheritance should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn local_inferred_properties_and_no_arg_super_constructor_publish_before_body_checking() {
    const SRC: &str = "fun box(): String {\n\
    open class K {\n\
        val o = \"O\"\n\
    }\n\
    class Bar : K() {\n\
        val k = \"K\"\n\
    }\n\
    return K().o + Bar().k\n\
}\n";
    assert_eq!(
        run(SRC).expect("complete local source ABIs publish before the enclosing body is checked"),
        "OK"
    );
}

#[test]
fn transitively_inherited_inferred_local_member_is_visible_in_the_enclosing_body() {
    const SRC: &str = "fun box(): String {\n\
    val result = \"OK\"\n\
    open class Local {\n\
        fun result() = result\n\
    }\n\
    open class Middle : Local()\n\
    class Derived : Middle()\n\
    return Derived().result()\n\
}\n";
    assert_eq!(
        run(SRC).expect("the active local-member union must include inherited checked members"),
        "OK"
    );
}

#[test]
fn local_secondary_constructor_uses_a_listed_superclass_header() {
    const SRC: &str = "open class Root(val value: String)\n\
fun box(): String {\n\
    val captured = \"OK\"\n\
    open class Base : Root {\n\
        constructor(): super(captured)\n\
    }\n\
    class Derived : Base {\n\
        constructor(): super()\n\
    }\n\
    return Derived().value\n\
}\n";
    assert_eq!(
        run(SRC).expect("a superclass listed without a header call keeps its finalized identity"),
        "OK"
    );
}

#[test]
fn named_local_object_is_rejected() {
    // Kotlin permits local anonymous-object expressions, but a named singleton declaration cannot
    // be local. Keep this at the frontend boundary: synthesizing JVM INSTANCE storage for it would
    // accept a program kotlinc rejects.
    const SRC: &str = "fun box(): String {\n\
    object Registry {\n\
        val tag = \"reg\"\n\
        fun describe() = \"id=\" + tag\n\
    }\n\
    if (Registry.describe() != \"id=reg\") return \"fail object\"\n\
    val anon = object { val n = 7 }\n\
    if (anon.n != 7) return \"fail anon\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        common::front_end_diagnostics(SRC, &[common::stdlib_jar()], Some(&common::jdk_modules())),
        ["named object 'Registry' cannot be local. Try to use an anonymous object instead."]
    );
}

#[test]
fn a_local_class_body_properties_keep_their_own_identities() {
    // A local class's body properties are numbered from the body; the legacy source coordinate
    // numbers the constructor's `val` parameters first. Reading one numbering as the other bound
    // every property reference to the NEXT declaration — `a` named `b` — so `val b = a + 1` saw an
    // unwritten field and `p.a` answered with `b`. Both fail only when the counts can collide, so
    // this covers one constructor `val` (where they did) and none (where they did not).
    const SRC: &str = "fun box(): String {\n\
    class P(val n: Int) {\n\
        val a = n * 2\n\
        val b = a + 1\n\
        val c = b + 1\n\
    }\n\
    class Q {\n\
        val a = 100\n\
        val b = a + 1\n\
    }\n\
    val p = P(3)\n\
    if (p.n != 3) return \"fail n=${p.n}\"\n\
    if (p.a != 6) return \"fail a=${p.a}\"\n\
    if (p.b != 7) return \"fail b=${p.b}\"\n\
    if (p.c != 8) return \"fail c=${p.c}\"\n\
    val q = Q()\n\
    if (q.a != 100 || q.b != 101) return \"fail q=${q.a},${q.b}\"\n\
    return \"OK\"\n\
}\n";
    // Both compilers must ACCEPT it, and both must answer the same. A krusty-only run would have
    // passed on the wrong numbering too, had the expected values been read off krusty instead of
    // reasoned from the source.
    let diagnostics = common::compiler_diagnostics(&[("Main.kt", SRC)], &[]);
    assert_eq!(
        (
            diagnostics.reference_code,
            diagnostics.reference_stderr.as_str()
        ),
        (0, ""),
        "kotlinc must accept the exact fixture without diagnostics",
    );
    assert_eq!(
        (diagnostics.krusty_code, diagnostics.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the same source without diagnostics",
    );
    assert_eq!(common::kotlinc_box_result(SRC), "OK");
    // Run the krusty-built classes too: unlike a verifier failure, the wrong property identity
    // merely reads the wrong valid field and is observable only in the returned value.
    common::expect_box_ok_with_stdlib(SRC, "LocalClassBodyPropertyIdentity");
}

#[test]
fn a_constructor_default_anonymous_object_overrides_its_local_class() {
    // The local class and the anonymous object live in a constructor-parameter default, which is
    // checked and lowered before ordinary bodies. The object's override is reachable only once
    // that object's applied hierarchy is published with its header.
    const SRC: &str = "\
fun <T> eval(fn: () -> T) = fn()\n\
class A(\n\
    val a: String = eval {\n\
        open class B {\n\
            open fun s(): String = \"O\"\n\
        }\n\
        val o = object : B() {\n\
            override fun s(): String = \"K\"\n\
        }\n\
        B().s() + o.s()\n\
    }\n\
)\n\
fun box(): String = A().a\n";
    common::expect_box_same_as_kotlinc(SRC, "LocalClassInParameterInitializer");
}

#[test]
fn a_body_local_interface_is_not_validated_as_a_superclass() {
    const SRC: &str = r#"// LANGUAGE: +SuspendFunctionAsSupertype
interface Operation : suspend (String) -> Int

fun operation(): Operation = object : Operation {
    override suspend fun invoke(value: String): Int = value.length
}

fun box(): String {
    operation()
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "BodyLocalInterfaceRole");
}

#[test]
fn a_body_local_final_superclass_keeps_the_exact_diagnostic() {
    const SRC: &str = "fun box(): String {\n\
    class Final\n\
    class Child : Final()\n\
    return \"OK\"\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", SRC)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let expected = [common::CompilerError {
        file: "Main.kt".to_string(),
        line: 3,
        column: 15,
        message: "this type is final, so it cannot be extended.".to_string(),
    }];
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), expected);
}

#[test]
fn an_explicit_body_local_property_contract_precedes_its_member_bodies() {
    const SRC: &str = r#"interface Holder {
    fun install(value: suspend (String) -> Unit)
    suspend fun fire()
}

fun holder(): Holder = object : Holder {
    var callback: suspend (String) -> Unit = {}
    override fun install(value: suspend (String) -> Unit) { callback = value }
    override suspend fun fire() { callback("K") }
}

fun box(): String {
    holder().install {}
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "BodyLocalExplicitProperty");
}

#[test]
fn a_retained_inline_body_waits_for_its_body_local_owner() {
    const SRC: &str = r#"fun add(left: Int, right: Int): Int {
    val holder = object {
        inline fun add(left: Int, right: Int): Int = left + right
    }
    return holder.add(left, right)
}

fun box(): String = if (add(1, 1) == 2) "OK" else "FAIL"
"#;
    common::expect_box_same_as_kotlinc(SRC, "BodyLocalRetainedInline");
}

#[test]
fn a_retained_default_waits_for_its_body_local_owner() {
    const SRC: &str = r#"fun box(): String {
    open class Base {
        open fun value(prefix: String, suffix: String = "K"): String = prefix + suffix
    }
    class Derived : Base() {
        override fun value(prefix: String, suffix: String): String =
            super.value(prefix, suffix)
    }
    return Derived().value("O")
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "BodyLocalRetainedDefault");
}

#[test]
fn a_body_local_extension_property_keeps_its_receiver() {
    const SRC: &str = r#"interface ExtensionBase { val String.tag: String }

fun box(): String {
    class Extension : ExtensionBase {
        override val String.tag: String get() = this
    }
    return with(Extension()) { "OK".tag }
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "BodyLocalExtensionProperty");
}
