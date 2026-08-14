//! A classifier and a same-named top-level factory function sharing one name (the neutral
//! `fun AxisSpacing(...): AxisSpacing` idiom): when no constructor is applicable — an
//! interface has none at all — kotlinc binds the call to the function. Constructor candidates
//! keep precedence whenever one DOES apply (kotlinc: an applicable abstract-class constructor
//! still errors, and a class's applicable constructor beats the function).

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

/// A named/default call where an interface and its top-level factory share a name. The fixture is
/// intentionally domain-neutral: regression tests must describe the semantic shape without retaining
/// identities from a private reproduction.
#[test]
fn named_args_pick_factory() {
    run_ok(
        "IfaceFactoryNamed",
        "interface AxisSpacing { val top: Int; val bottom: Int }\n\
         fun AxisSpacing(top: Int = 0, bottom: Int = 0): AxisSpacing = AxisSpacingImpl(top, bottom)\n\
         private class AxisSpacingImpl(val t: Int, val b: Int) : AxisSpacing {\n\
         \x20   override val top: Int get() = t\n\
         \x20   override val bottom: Int get() = b\n\
         }\n\
         fun box(): String {\n\
         \x20   val g = AxisSpacing(top = 4, bottom = 8)\n\
         \x20   return if (g.top == 4 && g.bottom == 8) \"OK\" else \"F\" }\n",
    );
}

/// `AxisSpacing()` (all defaults) and `AxisSpacing(4)` (positional) both pick the factory — an interface
/// has no constructor candidate at all, however the call is shaped.
#[test]
fn positional_and_default_args_pick_factory() {
    run_ok(
        "IfaceFactoryPositional",
        "interface AxisSpacing { val top: Int; val bottom: Int }\n\
         fun AxisSpacing(top: Int = 0, bottom: Int = 0): AxisSpacing = AxisSpacingImpl(top, bottom)\n\
         private class AxisSpacingImpl(val t: Int, val b: Int) : AxisSpacing {\n\
         \x20   override val top: Int get() = t\n\
         \x20   override val bottom: Int get() = b\n\
         }\n\
         fun box(): String {\n\
         \x20   val a = AxisSpacing(4)\n\
         \x20   val b = AxisSpacing()\n\
         \x20   return if (a.top == 4 && b.top == 0 && b.bottom == 0) \"OK\" else \"F\" }\n",
    );
}

/// The real declaration also carries a companion object; the factory still wins over the
/// classifier.
#[test]
fn factory_wins_over_companion_object() {
    run_ok(
        "IfaceFactoryCompanion",
        "interface AxisSpacing {\n\
         \x20   companion object { @JvmField val EMPTY: AxisSpacing = EmptyAxisSpacing }\n\
         \x20   val top: Int\n\
         }\n\
         fun AxisSpacing(top: Int = 0): AxisSpacing = AxisSpacingImpl(top)\n\
         private object EmptyAxisSpacing : AxisSpacing { override val top: Int = 0 }\n\
         private class AxisSpacingImpl(val t: Int) : AxisSpacing { override val top: Int get() = t }\n\
         fun box(): String {\n\
         \x20   val g = AxisSpacing(top = 4)\n\
         \x20   return if (g.top == 4 && AxisSpacing.EMPTY.top == 0) \"OK\" else \"F\" }\n",
    );
}

/// An interface WITHOUT a factory function stays an error, with the message unchanged.
#[test]
fn interface_without_factory_still_rejected() {
    let d = diags("interface NoFac { val x: Int }\nfun f() { val g = NoFac() }");
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        d.iter()
            .any(|m| m.contains("cannot create an instance of an interface 'NoFac'")),
        "expected the interface-instantiation diagnostic, got: {d:?}"
    );
}

/// kotlinc prefers the constructor when BOTH it and a same-named function apply: `Abs()` binds
/// the (abstract) constructor and errors, never the default-arg factory.
#[test]
fn abstract_class_applicable_ctor_still_rejected() {
    let d = diags(
        "abstract class Abs { abstract val x: Int }\n\
         fun Abs(x: Int = 1): Abs = AbsImpl(x)\n\
         private class AbsImpl(val v: Int) : Abs() { override val x: Int get() = v }\n\
         fun f() { val a = Abs() }",
    );
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        d.iter()
            .any(|m| m.contains("cannot create an instance of an abstract class 'Abs'")),
        "expected the abstract-instantiation diagnostic, got: {d:?}"
    );
}

/// A class whose constructor does NOT apply falls through to the same-named factory (kotlinc
/// binds `C("ab")` to the function); applicable constructor calls keep constructing.
#[test]
fn constructor_preferred_when_applicable() {
    run_ok(
        "CtorPreferredOverFactory",
        "class C(val x: Int = 0)\n\
         fun C(s: String): C = C(s.length)\n\
         fun box(): String {\n\
         \x20   val a = C(1)\n\
         \x20   val b = C(\"ab\")\n\
         \x20   val c = C()\n\
         \x20   return if (a.x == 1 && b.x == 2 && c.x == 0) \"OK\" else \"F\" }\n",
    );
}

/// An abstract class's constructor takes no `x`, so `Abs(5)` / `Abs(x = 7)` bind the factory
/// (kotlinc accepts exactly these shapes).
#[test]
fn abstract_class_factory() {
    run_ok(
        "AbstractFactory",
        "abstract class Abs { abstract val x: Int }\n\
         fun Abs(x: Int = 1): Abs = AbsImpl(x)\n\
         private class AbsImpl(val v: Int) : Abs() { override val x: Int get() = v }\n\
         fun box(): String = if (Abs(5).x == 5 && Abs(x = 7).x == 7) \"OK\" else \"F\"\n",
    );
}

/// When both fallback callables accept the arguments, Kotlin gives the top-level factory precedence
/// over the companion operator. This guards applicability-based ordering rather than an interface-only
/// shortcut: both declarations are real call candidates, and declaration order must not decide it.
#[test]
fn applicable_factory_precedes_applicable_companion_invoke() {
    run_ok(
        "FactoryBeforeCompanionInvoke",
        "interface Choice { val selected: String\n\
         \x20   companion object { operator fun invoke(value: Int): Choice = ChoiceImpl(\"companion\") }\n\
         }\n\
         fun Choice(value: Int): Choice = ChoiceImpl(\"factory\")\n\
         private class ChoiceImpl(override val selected: String) : Choice\n\
         fun box(): String = if (Choice(3).selected == \"factory\") \"OK\" else \"F\"\n",
    );
}

/// Mere factory existence is not enough to hide a companion operator. The same classifier name has a
/// String factory and an Int companion call; each argument shape must reach its applicable callable.
#[test]
fn inapplicable_factory_falls_through_to_companion_invoke() {
    run_ok(
        "InapplicableFactoryBeforeCompanion",
        "interface Choice { val selected: String\n\
         \x20   companion object { operator fun invoke(value: Int): Choice = ChoiceImpl(\"companion\") }\n\
         }\n\
         fun Choice(value: String): Choice = ChoiceImpl(\"factory:\" + value)\n\
         private class ChoiceImpl(override val selected: String) : Choice\n\
         fun box(): String {\n\
         \x20   val a = Choice(3).selected\n\
         \x20   val b = Choice(\"x\").selected\n\
         \x20   return if (a == \"companion\" && b == \"factory:x\") \"OK\" else \"F\"\n\
         }\n",
    );
}

#[test]
fn inapplicable_companion_invoke_reports_its_candidate() {
    let source = "interface Choice { companion object { operator fun invoke(value: Int): Choice = null as Choice } }\n\
                  fun use(): Choice = Choice(\"wrong\")\n";
    let diagnostics = common::front_end_diagnostics(source, &[], None);
    assert_eq!(
        diagnostics,
        ["argument type mismatch: actual type is 'String', but 'Int' was expected."],
        "the rejected companion operator must supply the final call diagnostic"
    );
}

/// A dependency-provided interface and a source factory use the same classifier/callable arbitration
/// as a source interface. The Java fixture is deliberately minimal: it proves that provider origin is
/// not a resolution branch, while the Kotlin implementation makes the selected factory observable.
#[test]
fn dependency_interface_uses_source_factory() {
    let java = [(
        "Metric.java".into(),
        "package fixtures; public interface Metric { int amount(); }".into(),
    )];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library, stdlib];
    let source = "import fixtures.Metric\n\
        private class MetricValue(val raw: Int) : Metric { override fun amount(): Int = raw }\n\
        fun Metric(raw: Int = 6): Metric = MetricValue(raw)\n\
        fun box(): String {\n\
        \x20   val positional = Metric(4)\n\
        \x20   val named = Metric(raw = 5)\n\
        \x20   val defaulted = Metric()\n\
        \x20   return if (positional.amount() + named.amount() + defaulted.amount() == 15) \"OK\" else \"F\"\n\
        }\n";
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

/// A same-named factory whose result uses a scalar value-class carrier must remain unboxed at the call
/// boundary. This is the representation-sensitive form of the same classifier/callable arbitration;
/// resolving it as a function is insufficient if lowering then treats its result as a boxed instance.
#[test]
fn value_class_factory_preserves_scalar_carrier() {
    run_ok(
        "ValueClassFactoryCarrier",
        "@JvmInline value class ScalarToken(val raw: Int)\n\
         fun ScalarToken(high: Int, low: Int): ScalarToken = ScalarToken((high shl 8) or low)\n\
         fun box(): String = if (ScalarToken(2, 3).raw == 515) \"OK\" else \"F\"\n",
    );
}

/// A factory result must also retain its unboxed carrier when it becomes the receiver of an extension.
/// This separates call-result representation from the indexed-container case below, making a failure at
/// either boundary identify the responsible lowering stage without relying on a reproduction class name.
#[test]
fn value_class_factory_composes_with_extension_call() {
    run_ok(
        "ValueClassFactoryExtensionCarrier",
        "@JvmInline value class PackedValue(val raw: Int) { val lower: Int get() = raw and 255 }\n\
         fun PackedValue(upper: Int, lower: Int): PackedValue = PackedValue((upper shl 8) or lower)\n\
         fun PackedValue.withUpper(upper: Int) = PackedValue(upper, lower)\n\
         fun box(): String = if (PackedValue(2, 3).withUpper(4).raw == 1027) \"OK\" else \"F\"\n",
    );
}

/// The factory result can flow through value-class extension calls and an indexed setter without being
/// mistaken for its scalar carrier. The member ABI boxes the value-class parameter, then its body must
/// unbox that parameter before reading the sole property; checking the backing array isolates precisely
/// that supported boundary from boxed value-class member returns, which this backend rejects safely.
#[test]
fn value_class_factory_composes_with_indexed_container() {
    run_ok(
        "ValueClassFactoryIndexedCarrier",
        "@JvmInline value class PackedValue(val raw: Int) {\n\
         \x20   val upper: Int get() = (raw shr 8) and 255\n\
         \x20   val lower: Int get() = raw and 255\n\
         }\n\
         fun PackedValue(upper: Int, lower: Int): PackedValue = PackedValue((upper shl 8) or lower)\n\
         fun PackedValue.withUpper(upper: Int) = PackedValue(upper, lower)\n\
         @JvmInline value class PackedValues(val data: IntArray) {\n\
         \x20   constructor(size: Int) : this(IntArray(size))\n\
         \x20   operator fun set(index: Int, value: PackedValue) { data[index] = value.raw }\n\
         }\n\
         fun box(): String {\n\
         \x20   val values = PackedValues(1)\n\
         \x20   values[0] = PackedValue(2, 3).withUpper(4)\n\
         \x20   return if (values.data[0] == 1027) \"OK\" else \"F\"\n\
         }\n",
    );
}

/// An indexed getter is a user member, not a stored-property getter merely because both JVM names begin
/// with `get`. User value-class members returning another value class still require a boxed-result model
/// that this backend does not implement, so the architecture contract is to decline this whole file. This
/// pins the safe, semantic classification: accepting and miscompiling it is worse than an explicit bail.
#[test]
fn indexed_getter_returning_value_class_is_rejected_safely() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let source = "@JvmInline value class ElementToken(val raw: Int)\n\
                  @JvmInline value class TokenBuffer(val data: IntArray) {\n\
                  \x20   operator fun get(index: Int): ElementToken = ElementToken(data[index])\n\
                  }\n\
                  fun box(): String = if (TokenBuffer(IntArray(1))[0].raw == 0) \"OK\" else \"F\"\n";
    assert_eq!(
        common::backend_rejects_in_process(
            source,
            "IndexedValueClassReturn",
            &[stdlib],
            Some(&jdk)
        ),
        Some(true),
    );
}
