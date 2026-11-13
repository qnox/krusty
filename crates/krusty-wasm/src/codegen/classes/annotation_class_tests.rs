use super::check_supported;
use krusty::ir::{ClassId, IrExpr, IrFile};
use krusty::types::type_name;

fn lower(source: &str) -> IrFile {
    krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        source,
        "AnnotationClasses",
    )
}

fn annotation(ir: &IrFile, name: &str) -> ClassId {
    ir.classes
        .iter()
        .position(|class| class.fq_name == type_name(name))
        .expect("the annotation class is lowered") as ClassId
}

fn constructions(ir: &IrFile, name: &str) -> usize {
    ir.exprs
        .iter()
        .filter(|expr| matches!(expr, IrExpr::New { internal, .. } if *internal == type_name(name)))
        .count()
}

#[test]
fn an_applied_annotation_class_is_laid_out() {
    let ir = lower(include_str!(
        "../../../../../tests/box_regressions/appliedAnnotationClass.kt"
    ));
    for name in ["Tag", "Marker"] {
        assert_eq!(
            check_supported(&ir, annotation(&ir, name)),
            Ok(()),
            "{name}"
        );
    }
}

/// A constructed annotation instance is laid out like any class. Its `equals`, `hashCode` and
/// `toString` have no body here, so the layout is the only obligation a construction adds.
#[test]
fn a_constructed_annotation_instance_is_laid_out() {
    let ir = lower(
        "annotation class Tag(val n: Int)\n\
         fun box(): String = if (Tag(1).n == 1) \"OK\" else \"fail\"\n",
    );
    assert_eq!(constructions(&ir, "Tag"), 1);
    for (id, class) in ir.classes.iter().enumerate() {
        assert_eq!(
            check_supported(&ir, id as ClassId),
            Ok(()),
            "{}",
            class.fq_name()
        );
    }
}
