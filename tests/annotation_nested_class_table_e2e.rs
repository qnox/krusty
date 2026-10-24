//! PLATFORM: JVM annotation interfaces publish their declared nested classifiers in InnerClasses.

use super::common;

#[test]
fn an_annotation_lists_an_unreferenced_declared_class() {
    let source = "annotation class Tag { class Child }\n";
    let classes = common::classes_against_kotlinc_module(&[("TopLevel.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_nested_annotation_lists_its_declared_class_and_enclosing_chain() {
    let source = "class Outer { class Middle { annotation class Tag { class Child } } }\n";
    let classes = common::classes_against_kotlinc_module(&[("Nested.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn annotation_member_classifiers_share_the_complete_nested_inventory() {
    let source = r#"annotation class Tag {
    object Token
    interface Marker
    enum class Mode { FIRST }
    annotation class Nested { class Leaf }
}
"#;
    let classes = common::classes_against_kotlinc_module(&[("Members.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}
