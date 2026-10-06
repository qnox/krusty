//! `value class` without `@JvmInline`.
//!
//! `+FullValueClasses` makes it a boxed class with structural `equals`/`hashCode`/`toString`.
//! Without the feature the declaration is rejected. `@JvmInline` (including an import alias)
//! keeps the unboxed inline ABI (`constructor-impl` / `box-impl` / `unbox-impl`).

use krusty::jvm::classreader::parse_class;

use super::common;

const ACC_PUBLIC: u16 = 0x0001;

fn compile(src: &str, stem: &str) -> Vec<(String, Vec<u8>)> {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    common::compile_in_process(
        src,
        stem,
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .unwrap_or_else(|| {
        panic!(
            "{stem}: {:?}",
            common::compile_in_process_diagnostics(src, stem, &[stdlib], Some(jdk.as_path()),)
        )
    })
}

fn class_bytes<'a>(classes: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
    classes
        .iter()
        .find(|(internal, _)| internal == name)
        .unwrap_or_else(|| panic!("{name} was not emitted"))
        .1
        .as_slice()
}

#[test]
fn value_class_without_jvm_inline_is_rejected_until_the_feature_is_enabled() {
    const SRC: &str = "value class A(val x: Int)\nfun box() = \"OK\"\n";
    assert_eq!(
        common::front_end_diagnostics_located(SRC, &[], None),
        vec![
            "1:1: error: value classes without '@JvmInline' annotation are not yet supported."
                .to_string()
        ]
    );
    common::assert_errors_match_kotlinc(&[("Value.kt", SRC)], &[]);
}

#[test]
fn full_value_class_matches_data_class_equality() {
    const SRC: &str = "\
// LANGUAGE: +FullValueClasses
value class A1(val x: Int)
value class A2(val x: Int, val y: Int)
data class A1_(val x: Int)
data class A2_(val x: Int, val y: Int)
class Outer {
    value class Inner(val x: Int, val y: Int)
}
fun box(): String {
    val a = A1(2)
    val a_ = A1_(2)
    if (a != A1(2)) return \"eq1\"
    if (a.x != a_.x) return \"x\"
    if (a.toString() != a_.toString().replace(\"_\", \"\")) return \"str \" + a.toString()
    if (a.hashCode() != a_.hashCode()) return \"hash\"
    val b = A2(2, 3)
    val b_ = A2_(2, 3)
    if (b != A2(2, 3)) return \"eq2\"
    if (b.x != b_.x || b.y != b_.y) return \"xy\"
    if (b.toString() != b_.toString().replace(\"_\", \"\")) return \"str2 \" + b.toString()
    if (b.hashCode() != b_.hashCode()) return \"hash2\"
    val inner = Outer.Inner(1, 2)
    if (inner != Outer.Inner(1, 2) || inner.toString() != \"Inner(x=1, y=2)\") return inner.toString()
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(SRC, "full_value_class_matches_data_class_equality");
    common::expect_box_same_as_kotlinc_with_args(
        SRC,
        "full_value_class_matches_data_class_equality_differential",
        &["-XXLanguage:+FullValueClasses"],
    );

    let classes = compile(SRC, "full_value_class_matches_data_class_equality");
    for (name, init) in [("A1", "(I)V"), ("A2", "(II)V"), ("Outer$Inner", "(II)V")] {
        let ci = parse_class(class_bytes(&classes, name)).expect(name);
        let constructor = ci
            .method("<init>", init)
            .unwrap_or_else(|| panic!("{name} is missing {init}"));
        assert_ne!(
            constructor.access & ACC_PUBLIC,
            0,
            "{name} constructor must be public"
        );
        for absent in [
            "constructor-impl",
            "box-impl",
            "unbox-impl",
            "component1",
            "copy",
        ] {
            assert!(
                ci.methods_named(absent).is_empty(),
                "{name} must not declare {absent}"
            );
        }
    }
}

#[test]
fn jvm_inline_value_class_stays_unboxed() {
    const SRC: &str = "\
@JvmInline
value class Id(val x: Int)
fun box(): String = \"OK\"
";
    let classes = compile(SRC, "jvm_inline_value_class_stays_unboxed");
    let ci = parse_class(class_bytes(&classes, "Id")).expect("Id");
    assert!(ci.method("constructor-impl", "(I)I").is_some());
    assert!(ci.method("box-impl", "(I)LId;").is_some());
    assert!(ci.method("unbox-impl", "()I").is_some());
}

#[test]
fn full_value_constructor_properties_are_visible_during_super_init() {
    const SRC: &str = "\
// LANGUAGE: +FullValueClasses
val log = mutableListOf<String>()
abstract value class Base {
    abstract val i: Int
    init { log.add(\"Base.init:i=$i\") }
}
value class Derived(override val i: Int, val f: Float) : Base() {
    init { log.add(\"Derived.init:i=$i,f=$f\") }
}
abstract value class Abs(a: Int) {
    init { log.add(\"this=\" + this.toString()) }
}
value class Child(val x: Int) : Abs(x * 3) {
    override fun toString(): String = \"Child($x)\"
}
fun box(): String {
    log.clear()
    val d = Derived(42, 3.14f)
    if (d.i != 42 || d.f != 3.14f) return \"fields\"
    if (log != listOf(\"Base.init:i=42\", \"Derived.init:i=42,f=3.14\")) return log.toString()
    log.clear()
    val c = Child(5)
    if (c.x != 5) return \"x\"
    if (log != listOf(\"this=Child(5)\")) return log.toString()
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(
        SRC,
        "full_value_constructor_properties_are_visible_during_super_init",
    );
}

#[test]
fn regular_subclass_of_abstract_value_class_uses_identity_equality() {
    const SRC: &str = "\
// LANGUAGE: +FullValueClasses
abstract value class AbstractValue(p0: Int, p1: Int) {
    abstract val p2: Int
}
class SimpleClass(p0: Int, val p1: Int) : AbstractValue(p0, p1) {
    override val p2: Int get() = p1
}
fun box(): String {
    val a = SimpleClass(1, 2)
    val b = SimpleClass(1, 2)
    if (a == b) return \"eq\"
    if (a.hashCode() == b.hashCode()) return \"hash\"
    if (a.toString() == b.toString()) return \"str\"
    if (a.p2 != 2) return \"p2\"
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(
        SRC,
        "regular_subclass_of_abstract_value_class_uses_identity_equality",
    );
    let classes = compile(
        SRC,
        "regular_subclass_of_abstract_value_class_uses_identity_equality",
    );
    let parent = parse_class(class_bytes(&classes, "AbstractValue")).expect("AbstractValue");
    for absent in ["equals", "hashCode", "toString"] {
        assert!(
            parent.methods_named(absent).is_empty(),
            "AbstractValue must not declare {absent}"
        );
    }
}

#[test]
fn final_value_subclass_keeps_structural_tostring() {
    const SRC: &str = "\
// LANGUAGE: +FullValueClasses
abstract value class Base
value class Child(val x: Int) : Base()
fun box(): String {
    val child = Child(1)
    if (child.toString() != \"Child(x=1)\") return child.toString()
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(SRC, "final_value_subclass_keeps_structural_tostring");
}

#[test]
fn full_value_class_accepts_unit_and_nothing_carriers() {
    const SRC: &str = "\
// LANGUAGE: +FullValueClasses
value class UnitWrapper(val unit: Unit)
value class NothingWrapper(val nothing: Nothing)
value class UnitWrapper1<T: Unit>(val unit: T)
value class NothingWrapper1<T: Nothing>(val nothing: T)
fun box(): String {
    val unitWrapper = UnitWrapper(Unit)
    if (unitWrapper != UnitWrapper(Unit)) return \"eq\"
    if (unitWrapper.unit != Unit) return \"unit\"
    val unitWrapper1 = UnitWrapper1(Unit)
    if (unitWrapper1 != UnitWrapper1(Unit)) return \"eq1\"
    if (unitWrapper1.unit != Unit) return \"unit1\"
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(SRC, "full_value_class_accepts_unit_and_nothing_carriers");
}

#[test]
fn full_value_secondary_constructor_may_omit_a_default() {
    const SRC: &str = "\
// LANGUAGE: +FullValueClasses
value class Successful(val x: Int = 1, val y: Int = 2) {
    constructor() : this(8)
}
fun box(): String {
    val value = Successful()
    if (value.x != 8 || value.y != 2) return value.toString()
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(SRC, "full_value_secondary_constructor_may_omit_a_default");
}

#[test]
fn aliased_jvm_inline_annotation_stays_unboxed() {
    const SRC: &str = "\
import kotlin.jvm.JvmInline as Inline
@Inline
value class Id(val x: Int)
fun box(): String = \"OK\"
";
    let classes = compile(SRC, "aliased_jvm_inline_annotation_stays_unboxed");
    let ci = parse_class(class_bytes(&classes, "Id")).expect("Id");
    assert!(ci.method("constructor-impl", "(I)I").is_some());
    assert!(ci.method("box-impl", "(I)LId;").is_some());
    assert!(ci.method("unbox-impl", "()I").is_some());
}
