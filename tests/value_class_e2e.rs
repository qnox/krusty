//! Value/inline-class member synthesis (phase 388). A `@JvmInline value class X(val v: U)` emits
//! kotlinc's unboxed-support members on `X.class`: the `U` field + `<init>(U)` + `getV()` from the
//! ordinary single-field class path, plus the synthesized `box-impl(U):X` / `constructor-impl(U):U`
//! (static) and `unbox-impl():U` (instance). Use-site unboxing is wired (value-class params/fields/
//! construction lower to the unboxed underlying type — see tests/session_subsystems_e2e.rs::
//! value_class_unboxed_arith), so `check_file` accepts value-class files; this test drives the library
//! directly to verify the synthesized class shape — the structural half of the differential-vs-kotlinc
//! check.

use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures_with_cp};
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::classreader::parse_class;
use krusty::jvm::ir_emit::emit_all;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::names::file_class_name;
use krusty::lexer::lex;
use krusty::parser::parse;

use super::common;

const ACC_STATIC: u16 = 0x0008;

#[test]
fn value_class_synthesizes_box_unbox_constructor_impl() {
    let src = "@JvmInline\nvalue class S(val x: Int)\nfun box(): String = \"OK\"\n";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    assert!(!d.has_errors(), "unexpected parse errors");

    // `check_file` accepts value-class files (use-site unboxing is wired); the file resolves clean.
    let cp = std::rc::Rc::new(Classpath::new(vec![common::stdlib_jar()]));
    let mut syms =
        collect_signatures_with_cp(&files, Box::new(JvmLibraries::new(cp.clone())), &mut d);
    let info = check_file(&files[0], &mut syms, &mut d);
    assert!(!d.has_errors(), "value-class file should check clean");

    let runtime = JvmLibraries::new(cp.clone());
    let mut ir = lower_file(&files[0], &info, &syms, &runtime).expect("value class should lower");
    let facade = file_class_name("S", None);
    // The value-class `-impl` members are synthesized by the JVM passes (not `ir_lower`).
    krusty::jvm::backend::run_backend_passes(&mut ir, &files[0], &facade, "main", &syms, &cp)
        .expect("backend passes should accept this value class");
    let classes = emit_all(&ir, &facade, &*cp, None, &syms).expect("emit");

    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "S")
        .expect("S.class emitted");
    let ci = parse_class(bytes).expect("parse S.class");

    // box-impl(I)LS;  — static factory wrapping the underlying value.
    let box_impl = ci.method("box-impl", "(I)LS;").expect("box-impl(I)LS;");
    assert_ne!(
        box_impl.access & ACC_STATIC,
        0,
        "box-impl must be ACC_STATIC"
    );

    // constructor-impl(I)I  — static, returns the (validated) underlying value.
    let ctor_impl = ci
        .method("constructor-impl", "(I)I")
        .expect("constructor-impl(I)I");
    assert_ne!(
        ctor_impl.access & ACC_STATIC,
        0,
        "constructor-impl must be ACC_STATIC"
    );

    // unbox-impl()I  — instance method reading the field.
    let unbox = ci.method("unbox-impl", "()I").expect("unbox-impl()I");
    assert_eq!(
        unbox.access & ACC_STATIC,
        0,
        "unbox-impl is an instance method"
    );

    // The ordinary single-field class path still provides the field's getter.
    assert!(ci.method("getX", "()I").is_some(), "getX()I getter");

    // The static `-impl` members must NOT leak onto the top-level facade.
    if let Some((_, fbytes)) = classes.iter().find(|(n, _)| *n == facade) {
        let fc = parse_class(fbytes).expect("parse facade");
        assert!(
            fc.methods_named("box-impl").is_empty(),
            "box-impl must live on S, not the facade"
        );
    }
}

#[test]
fn value_class_is_property_uses_javabean_getter_name() {
    let src = "@JvmInline\nvalue class Flag(val isOpen: Boolean)\nfun box(): String = \"OK\"\n";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    assert!(!d.has_errors(), "unexpected parse errors");

    let cp = std::rc::Rc::new(Classpath::new(vec![common::stdlib_jar()]));
    let mut syms =
        collect_signatures_with_cp(&files, Box::new(JvmLibraries::new(cp.clone())), &mut d);
    let info = check_file(&files[0], &mut syms, &mut d);
    assert!(!d.has_errors(), "value-class file should check clean");

    let runtime = JvmLibraries::new(cp.clone());
    let mut ir = lower_file(&files[0], &info, &syms, &runtime).expect("value class should lower");
    let facade = file_class_name("Flag", None);
    krusty::jvm::backend::run_backend_passes(&mut ir, &files[0], &facade, "main", &syms, &cp)
        .expect("backend passes should accept this value class");
    let classes = emit_all(&ir, &facade, &*cp, None, &syms).expect("emit");

    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "Flag")
        .expect("Flag.class emitted");
    let ci = parse_class(bytes).expect("parse Flag.class");

    assert!(
        ci.method("isOpen", "()Z").is_some(),
        "isOpen boolean property getter"
    );
    assert!(
        ci.method("getIsOpen", "()Z").is_none(),
        "value-class is-property must not emit getIsOpen"
    );
}

#[test]
fn value_class_reference_underlying_eq_hash_to_string_runs() {
    let stdlib = common::stdlib_jar();
    let java_home = common::java_home();
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
@JvmInline
value class Id(val raw: String)

fun box(): String {
    val a = Id("x")
    if (a != Id("x")) return "f1"
    if (a == Id("y")) return "f2"
    if (a.hashCode() != Id("x").hashCode()) return "f3"
    if (a.toString() != "Id(raw=x)") return "f4:$a"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "IdBox", &[stdlib], Some(&jdk)).as_deref(),
        Some("OK")
    );
}

#[test]
fn value_class_secondary_constructor_may_delegate_through_a_sibling() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let src = r#"
// LANGUAGE: +ValueClassesSecondaryConstructorWithBody
@JvmInline
value class Wrapped(val value: String) {
    constructor(value: Int) : this(value.toString())
    constructor(value: Double) : this(value.toInt())
}

fun box(): String = Wrapped(42.0).value
"#;
    assert_eq!(
        common::compile_and_run_box(src, "ValueClassSecondaryChain", &[stdlib], Some(&jdk))
            .as_deref(),
        Some("42")
    );
}

#[test]
fn value_class_secondary_constructor_does_not_reframe_a_nested_lambda() {
    common::expect_box_ok_with_stdlib(
        r#"
// LANGUAGE: +ValueClassesSecondaryConstructorWithBody
val seen = mutableListOf<Double>()

@JvmInline
value class Wrapped(val value: Int) {
    constructor(value: Double) : this(value.toInt()) {
        seen.add(value.let(fun(item: Double) = item - 1.0))
    }
}

fun box(): String {
    Wrapped(3.0)
    return if (seen == listOf(2.0)) "OK" else seen.toString()
}
"#,
        "ValueClassSecondaryNestedLambda",
    );
}

#[test]
fn value_class_member_calls_in_retained_inline_lambda_use_the_static_carrier_abi() {
    common::expect_box_ok_with_stdlib(
        r#"
// LANGUAGE: +JvmInlineMultiFieldValueClasses
@JvmInline
value class Item(val value: Int)

@JvmInline
value class Items(val storage: IntArray) {
    fun contains(element: Item): Boolean = storage.contains(element.value)
    fun containsAll(elements: Collection<Item>): Boolean =
        elements.all { contains(it) }
}

fun box(): String =
    if (Items(intArrayOf(1, 2)).containsAll(listOf(Item(1), Item(2)))) "OK" else "Fail"
"#,
        "ValueClassRetainedInlineLambdaMemberCall",
    );
}

#[test]
fn value_class_member_default_call_uses_the_static_impl_stub() {
    common::expect_box_ok_with_stdlib(
        r#"
@JvmInline
value class Wrapped(val value: Int) {
    fun plus(other: Int = 42): Int = value + other
}

fun box(): String =
    if (Wrapped(800).plus() == 842 && Wrapped(400).plus(32) == 432) "OK" else "fail"
"#,
        "ValueClassMemberDefaultStub",
    );
}

#[test]
fn nullable_reference_underlying_value_class_extension_to_string_is_null_safe() {
    let stdlib = common::stdlib_jar();
    let java_home = common::java_home();
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
@JvmInline
value class Id(val raw: String)

fun Id?.show(): String = toString()

fun box(): String {
    val n = (null as Id?).show()
    if (n != "null") return "n:$n"
    val x = Id("x").show()
    if (x != "Id(raw=x)") return "x:$x"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "NullableValueClassToString", &[stdlib], Some(&jdk))
            .as_deref(),
        Some("OK")
    );
}

/// A generic inline extension's receiver is physically its first erased argument. This exercises the
/// shared callable-slot coercion with a USER value class rather than an unsigned builtin: specializing
/// `T.let` to `Ticket` leaves the logical receiver as `Ticket` while the JVM descriptor still takes
/// `Object`. The receiver must therefore be boxed through `Ticket.box-impl` before inline routing;
/// boxing only the `Int` carrier would verify but fail the spliced lambda's `Ticket` cast.
///
/// Keeping a non-builtin case beside the unsigned regressions prevents the representation fix from
/// shrinking back into a classifier list or unsigned-only emitter branch.
#[test]
fn generic_inline_scope_receiver_uses_the_declared_value_class_box() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline value class Ticket(val raw: Int)\n\
         fun make(): Ticket = Ticket(7)\n\
         fun box(): String = make().let { if (it.raw == 7) \"OK\" else \"bad\" }\n",
        "GenericInlineValueClassReceiver",
    );
}

#[test]
fn assignment_to_nullable_value_class_var_boxes() {
    let stdlib = common::stdlib_jar();
    let java_home = common::java_home();
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    // `w = it` / `u = W(7)`: a non-null value class assigned into a nullable (boxed) slot must
    // `box-impl` at the ASSIGNMENT boundary (`SetValue`), not only at a declaration initializer.
    // The captured `w` exercises the closure (`ObjectRef`) store path; `u` the plain local slot.
    let src = r#"
@JvmInline
value class W(val v: Int)

fun box(): String {
    var w: W? = null
    val cb: (W) -> Unit = { w = it }
    cb(W(42))
    if (w!!.v != 42) return "capture:${w!!.v}"
    var u: W? = null
    u = W(7)
    if (u!!.v != 7) return "local:${u!!.v}"
    val r: Result<Int>? = null
    var res: Result<Int>? = r
    res = Result.success(9)
    if (res!!.getOrNull() != 9) return "result"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "NullableValueClassAssign", &[stdlib], Some(&jdk))
            .as_deref(),
        Some("OK")
    );
}

#[test]
fn sized_array_of_value_class_uses_provider_value_underlying() {
    let stdlib = common::stdlib_jar();
    let java_home = common::java_home();
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = r#"
@JvmInline
value class Vc(val v: Int)

fun box(): String {
    val arr = Array(3) { Vc(it + 1) }
    var sum = 0
    for (x in arr) sum += x.v
    if (sum != 6) return "f1:$sum"
    if (arr[2].v != 3) return "f2"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "SizedValueClassArray", &[stdlib], Some(&jdk)).as_deref(),
        Some("OK")
    );
}

#[test]
fn top_level_and_nested_reference_arrays_keep_value_class_boxes() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline value class Scalar(val value: Int)\n\
         @JvmInline value class Data(val values: Array<UInt>)\n\
         val scalars = Array(2) { Scalar(42) }\n\
         val nested = Array(2) { i -> Data(Array(2) { j -> (i + j).toUInt() }) }\n\
         fun box(): String {\n\
             scalars[0] = Scalar(12)\n\
             if (scalars[0].value != 12) return \"scalar\"\n\
             if (nested[1].values[1].toInt() != 2) return \"nested\"\n\
             return \"OK\"\n\
         }\n",
        "NestedValueClassReferenceArrays",
    );
}

#[test]
fn covariant_override_unboxed_return_is_check() {
    let stdlib = common::stdlib_jar();
    let java_home = common::java_home();
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    // `t.bar()` resolves to the MANGLED interface method (value-class return convention), so `tBar`
    // is the unboxed underlying; `tBar is X` must box before `instanceof X`.
    let src = r#"
@JvmInline
value class X(val x: Any)

interface IBar {
    fun bar(): Any
}

interface IFoo : IBar {
    override fun bar(): X
}

class TestX : IFoo {
    override fun bar(): X = X("K")
}

fun box(): String {
    val t: IFoo = TestX()
    val tBar = t.bar()
    if (tBar !is X) return "f1:$tBar"
    return "OK"
}
"#;
    assert_eq!(
        common::compile_and_run_box(src, "CovariantOverrideUnboxedIs", &[stdlib], Some(&jdk))
            .as_deref(),
        Some("OK")
    );
}

#[test]
fn callable_reference_keeps_a_nullable_value_class_parameter_boxed() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline value class Wrapped(val value: Int)\n\
         var result: Int? = 0\n\
         object Target { fun accept(value: Wrapped?) { result = value?.value } }\n\
         fun box(): String {\n\
             Wrapped(42).let(Target::accept)\n\
             if (result != 42) return \"value:$result\"\n\
             null.let(Target::accept)\n\
             return if (result == null) \"OK\" else \"null:$result\"\n\
         }\n",
        "NullableValueClassCallableReference",
    );
}

#[test]
fn generic_underlying_metadata_keeps_the_owners_type_parameter() {
    common::expect_box_ok_with_stdlib(
        "inline class ICInt<T : Int>(val value: T)\n\
         inline class ICIcInt<T : ICInt<Int>>(val value: T)\n\
         fun box(): String {\n\
             if (ICInt(1).value != 1) return \"first\"\n\
             return if (ICIcInt(ICInt(1)).value.value == 1) \"OK\" else \"second\"\n\
         }\n",
        "GenericUnderlyingMetadata",
    );
}

#[test]
fn generic_interface_delegation_publishes_the_forwarders_type_parameter() {
    common::expect_box_ok_with_stdlib(
        "inline class S<T : String>(val value: T)\n\
         interface Consumer<T> { fun <X> consume(value: T, suffix: X): String }\n\
         object Impl : Consumer<S<String>> {\n\
             override fun <X> consume(value: S<String>, suffix: X): String =\n\
                 value.value + suffix.toString()\n\
         }\n\
         class Delegating : Consumer<S<String>> by Impl\n\
         fun box(): String = Delegating().consume(S(\"O\"), \"K\")\n",
        "GenericValueClassInterfaceDelegation",
    );
}

#[test]
fn generic_value_class_member_result_is_unboxed_before_underlying_property_read() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline\n\
         value class InlinedBase<T : Int>(val x: T) : Base<InlinedBase<T>> {\n\
             override fun Base<InlinedBase<T>>.foo(\n\
                 a: Base<InlinedBase<T>>,\n\
                 b: InlinedBase<T>,\n\
             ): Base<InlinedBase<T>> =\n\
                 if (a is InlinedBase<*>) InlinedBase((a.x + b.x) as T) else this\n\
             fun double(): InlinedBase<T> = this.foo(this, this) as InlinedBase<T>\n\
         }\n\
         interface Base<T> {\n\
             fun Base<T>.foo(a: Base<T>, b: T): Base<T>\n\
         }\n\
         fun box(): String {\n\
             val b = InlinedBase(3)\n\
             return if (b.double().x == 6) \"OK\" else \"Fail\"\n\
         }\n",
        "GenericValueClassMemberResultProperty",
    );
}

#[test]
fn value_class_override_bridge_invokes_the_static_carrier_member() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline\n\
         value class ComparableValue(val value: Int) : Comparable<ComparableValue> {\n\
             override fun compareTo(other: ComparableValue): Int = value - other.value\n\
         }\n\
         fun <T> compare(left: Comparable<T>, right: T): Int = left.compareTo(right)\n\
         fun box(): String =\n\
             if (compare(ComparableValue(4), ComparableValue(3)) == 1) \"OK\" else \"Fail\"\n",
        "ValueClassStaticCarrierBridge",
    );
}

#[test]
fn non_value_class_override_returns_the_unboxed_value_class_carrier() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline\n\
         value class Wrap(val s: String)\n\
         interface Base { fun get(): Wrap }\n\
         class Impl(val w: Wrap) : Base { override fun get(): Wrap = w }\n\
         fun box(): String {\n\
             val base: Base = Impl(Wrap(\"OK\"))\n\
             return base.get().s\n\
         }\n",
        "NonValueClassOverrideCarrierReturn",
    );
}

#[test]
fn value_class_computed_property_override_has_one_carrier_implementation() {
    common::expect_box_ok_with_stdlib(
        "interface Base { val id: Int }\n\
         @JvmInline\n\
         value class Child(val stored: Int) : Base {\n\
             override val id: Int get() = stored\n\
         }\n\
         fun box(): String {\n\
             val base: Base = Child(5)\n\
             return if (base.id == 5) \"OK\" else \"Fail\"\n\
         }\n",
        "ValueClassComputedPropertyOverride",
    );
}

#[test]
fn value_class_member_safe_cast_observes_the_boxed_interface_argument() {
    common::expect_box_ok_with_stdlib(
        "interface Marker\n\
         var sink: Any? = null\n\
         @JvmInline\n\
         value class Wrapped(val value: Int) : Marker {\n\
             fun save(other: Marker) { sink = (other as? Wrapped)?.value }\n\
         }\n\
         fun box(): String {\n\
             val value = Wrapped(5)\n\
             value.save(value)\n\
             return if (sink == 5) \"OK\" else \"Fail:$sink\"\n\
         }\n",
        "ValueClassMemberSafeCast",
    );
}

#[test]
fn synthesized_metadata_uses_properties_not_erased_value_class_fields() {
    common::expect_box_ok_with_stdlib(
        "inline class V<T : Int>(val value: T)\n\
         data class Data(val item: V<Int>)\n\
         class Entry : Map.Entry<V<Int>, V<Int>> {\n\
             override val key: V<Int> get() = V(2)\n\
             override val value: V<Int> get() = V(3)\n\
         }\n\
         fun box(): String {\n\
             if (Data(V(1)).component1().value != 1) return \"component\"\n\
             val entry: Map.Entry<V<Int>, V<Int>> = Entry()\n\
             return if (entry.key.value + entry.value.value == 5) \"OK\" else \"entry\"\n\
         }\n",
        "SemanticPropertyMetadata",
    );
}
