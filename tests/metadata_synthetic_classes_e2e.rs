//! `@Metadata` of classes kotlinc generates for an expression, measured against kotlinc 2.4.20.
//!
//! A callable-reference class (property, bound, mutable, top-level, function, bound function and
//! adapted references) and an annotation-instantiation class carry the `k=3` synthetic-class record
//! with LOCAL visibility packed into `xi`, and no `d1`/`d2`.

use super::common;

const REFERENCES: &str = "package app\n\
    \n\
    class Box(val value: Int) {\n\
    \x20   var mutable: Int = 0\n\
    \x20   fun twice(x: Int): Int = x + x\n\
    }\n\
    \n\
    var counter: Int = 0\n\
    val fixed: Int = 1\n\
    \n\
    annotation class Tag(val name: String)\n\
    \n\
    fun defaulted(x: Int, y: Int = 2): Int = x + y\n\
    \n\
    class Sink { fun keep(vararg values: Any) {} }\n\
    \n\
    fun references(box: Box, sink: Sink) {\n\
    \x20   val adapted: (Int) -> Int = ::defaulted\n\
    \x20   sink.keep(Box::value, Box::mutable, box::value, ::counter, ::fixed, Box::twice, box::twice, adapted, Tag(\"t\"))\n\
    }\n";

fn assert_identical(stem: &str, class_tail: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    let class = format!("app/{stem}Kt${class_tail}");
    common::metadata_header_diff_against_kotlinc_cp(stem, REFERENCES, &class, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_property_reference_class_is_a_local_synthetic_class() {
    assert_identical("PropertyReference", "references$1");
}

#[test]
fn a_mutable_property_reference_class_is_a_local_synthetic_class() {
    assert_identical("MutableReference", "references$2");
}

#[test]
fn a_bound_property_reference_class_is_a_local_synthetic_class() {
    assert_identical("BoundReference", "references$3");
}

#[test]
fn a_top_level_property_reference_class_is_a_local_synthetic_class() {
    assert_identical("TopLevelReference", "references$4");
}

#[test]
fn a_function_reference_class_is_a_local_synthetic_class() {
    assert_identical("FunctionReference", "references$6");
}

#[test]
fn a_bound_function_reference_class_is_a_local_synthetic_class() {
    assert_identical("BoundFunctionReference", "references$7");
}

#[test]
fn an_adapted_function_reference_class_is_a_local_synthetic_class() {
    assert_identical("AdaptedReference", "references$adapted$1");
}

#[test]
fn an_annotation_instantiation_class_is_a_local_synthetic_class() {
    assert_identical("AnnotationInstance", "annotationImpl$app_Tag$0");
}
