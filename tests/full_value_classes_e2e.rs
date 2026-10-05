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
    common::compile_in_process(src, stem, &[stdlib.clone()], Some(jdk.as_path())).unwrap_or_else(
        || {
            panic!(
                "{stem}: {:?}",
                common::compile_in_process_diagnostics(src, stem, &[stdlib], Some(jdk.as_path()),)
            )
        },
    )
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
        common::front_end_diagnostics(SRC, &[], None),
        vec!["value classes without '@JvmInline' annotation are not yet supported.".to_string()]
    );
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
