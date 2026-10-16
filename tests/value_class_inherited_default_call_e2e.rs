//! A call on a value class to an interface default it inherits without overriding takes the
//! class's static `name-impl(carrier, …)`, as kotlinc's value-class lowering replaces the fake
//! override with it. krusty boxed the carrier and dispatched through the interface instead.

use super::common;

const SRC: &str = "interface I {\n\
    \x20   val x: Int\n\
    \x20   fun doubled(): Int = x * 2\n\
    \x20   fun add(n: Int): Int = x + n\n\
    \x20   val y: Int get() = x + 1\n\
    \x20   var z: Int\n\
    \x20       get() = x + 2\n\
    \x20       set(value) {}\n\
    }\n\
    interface G<T> { fun echo(t: T): T = t }\n\
    @JvmInline value class V(override val x: Int) : I, G<String> { fun inside() = doubled() }\n\
    @JvmInline value class S(val s: String) : I { override val x: Int get() = s.length }\n\
    fun function(v: V) = v.doubled()\n\
    fun argument(v: V) = v.add(3)\n\
    fun getter(v: V) = v.y\n\
    fun setter(v: V) { v.z = 7 }\n\
    fun specialized(v: V) = v.echo(\"q\")\n\
    fun safe(v: V?) = v?.doubled()\n\
    fun reference(s: S) = s.doubled()\n\
    fun supertype(i: I) = i.doubled()\n\
    fun box(): String {\n\
    \x20   val v = V(21)\n\
    \x20   setter(v)\n\
    \x20   val total = function(v) + argument(v) + getter(v) + v.inside() + reference(S(\"ab\"))\n\
    \x20   if (total != 42 + 24 + 22 + 42 + 4) return \"total \" + total\n\
    \x20   if (specialized(v) != \"q\" || safe(null) != null || safe(v) != 42) return \"fail\"\n\
    \x20   return if (supertype(v) == 42) \"OK\" else \"supertype\"\n\
    }\n";

const CALLERS: &[&str] = &[
    "public static final int function-Wv3fJ18(int)",
    "public static final int argument-Wv3fJ18(int)",
    "public static final int getter-Wv3fJ18(int)",
    "public static final void setter-Wv3fJ18(int)",
    "public static final java.lang.String specialized-Wv3fJ18(int)",
    "public static final java.lang.Integer safe-X7kSWIk(V)",
    "public static final int reference-gSa4wCw(java.lang.String)",
    "public static final int supertype(I)",
];

#[test]
fn a_value_class_receiver_calls_its_inherited_default_static() {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassInheritedDefaultCall",
        SRC,
        "ValueClassInheritedDefaultCallKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for method in CALLERS {
        let reference = common::method_instructions(&built.reference, method);
        assert!(!reference.is_empty(), "kotlinc writes {method}");
        assert_eq!(
            common::method_instructions(&built.krusty, method),
            reference,
            "{method}"
        );
    }
}

#[test]
fn a_value_class_member_calls_its_own_inherited_default_static() {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassInheritedDefaultInside",
        SRC,
        "V",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public static final int inside-impl(int)";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn inherited_default_calls_on_value_classes_run() {
    common::expect_box_same_as_kotlinc(SRC, "ValueClassInheritedDefaultCall");
}

#[test]
fn a_sibling_value_class_uses_its_inherited_default_statics_from_the_caller_file() {
    const DECLARATIONS: &str = "package sibling\n\
        interface I {\n\
        \x20   fun doubled(): Int = 42\n\
        \x20   var answer: Int\n\
        \x20       get() = 42\n\
        \x20       set(value) {}\n\
        }\n\
        @JvmInline value class V(val value: Int) : I\n";
    const CALLER: &str = "package sibling\n\
        fun box(): String {\n\
        \x20   val value = V(1)\n\
        \x20   value.answer = 7\n\
        \x20   return if (value.doubled() == 42 && value.answer == 42) \"OK\" else \"fail\"\n\
        }\n";
    let sources = [("Declarations.kt", DECLARATIONS), ("Caller.kt", CALLER)];
    let pair = common::ModuleClassPair::compile(&sources, "sibling/CallerKt");
    let (reference, krusty) = pair.method_code("sibling/CallerKt", "box");
    assert_eq!(krusty, reference);
    common::expect_box_ok_files_with_stdlib(&sources, "sibling inherited-default calls");
}

#[test]
fn dependency_property_targets_select_the_value_class_inherited_default_statics() {
    const LIBRARY: &str = "package dependency\n\
        interface I {\n\
        \x20   var answer: Int\n\
        \x20       get() = 42\n\
        \x20       set(value) {}\n\
        }\n";
    const MAIN: &str = "package consumer\n\
        import dependency.I\n\
        @JvmInline value class V(val value: Int) : I\n\
        fun box(): String {\n\
        \x20   val value = V(1)\n\
        \x20   value.answer = 7\n\
        \x20   return if (value.answer == 42) \"OK\" else \"fail\"\n\
        }\n";
    let library = common::kotlinc_library(LIBRARY).expect("reference dependency compiles");
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("Main.kt", MAIN)],
        std::slice::from_ref(&library),
        "consumer/MainKt",
    );
    let (reference, krusty) = pair.method_code("consumer/MainKt", "box");
    assert_eq!(krusty, reference);

    let jdk = common::jdk_modules();
    let result = common::compile_and_run_box(
        MAIN,
        "Main",
        &[library, common::stdlib_jar(), jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(result.as_deref(), Some("OK"));
}
