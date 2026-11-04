use super::check_supported;
use crate::ir::{ClassId, IrFile};
use crate::types::type_name;

fn lower(source: &str) -> IrFile {
    crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(source, "AnnotationClasses")
}

fn annotation(ir: &IrFile, name: &str) -> ClassId {
    ir.classes
        .iter()
        .position(|class| class.fq_name == type_name(name))
        .expect("the annotation class is lowered") as ClassId
}

#[test]
fn an_applied_annotation_class_is_laid_out() {
    let ir = lower(
        "annotation class Tag(val n: Int)\n\
         @Tag(1) class Marked\n\
         fun box(): String = if (Marked() is Marked) \"OK\" else \"fail\"\n",
    );
    assert_eq!(check_supported(&ir, annotation(&ir, "Tag")), Ok(()));
}

#[test]
fn a_constructed_annotation_instance_is_declined() {
    let ir = lower(
        "annotation class Tag(val n: Int)\n\
         fun box(): String = if (Tag(1) == Tag(1)) \"OK\" else \"fail\"\n",
    );
    assert_eq!(
        check_supported(&ir, annotation(&ir, "Tag")),
        Err("an annotation instance (`Tag`)".to_string())
    );
}
