//! kotlinc generates one implementation class per instantiated annotation, named after the class
//! or file facade whose code instantiates it (`Owner$annotationImpl$Tag$0`). The class is anonymous
//! and synthetic: `InnerClasses` lists it with no outer class and no simple name, both in the
//! owner and in the class itself, and its `EnclosingMethod` names the owner with no method.
use super::common;

const SOURCE: &str = "annotation class Tag(val text: String)\n\
annotation class Mark(val level: Int)\n\
fun top(): Any = Tag(\"a\")\n\
class Maker {\n\
\x20   class Part\n\
\x20   fun make(): Any = Mark(1)\n\
\x20   fun part(): Any = Part()\n\
}\n";

#[test]
fn an_annotation_instance_class_is_listed_as_anonymous_and_synthetic() {
    common::assert_same_inner_classes(
        "AnnotationInstances",
        SOURCE,
        &[],
        &[
            "AnnotationInstancesKt",
            "AnnotationInstancesKt$annotationImpl$Tag$0",
            "Maker",
            "Maker$annotationImpl$Mark$0",
        ],
    );
}

/// With its `EnclosingMethod` naming the owner and no method, each implementation class is
/// byte-for-byte kotlinc's.
#[test]
fn an_annotation_instance_class_is_enclosed_by_its_owner_with_no_method() {
    let classes = [
        "AnnotationInstancesKt$annotationImpl$Tag$0",
        "Maker$annotationImpl$Mark$0",
    ];
    for (class, (expected, actual)) in classes.iter().zip(common::compile_with_kotlinc(
        "AnnotationInstances",
        SOURCE,
        &[],
        &classes,
    )) {
        assert!(actual == expected, "{class} differs from kotlinc");
    }
}
