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

fn function(ir: &IrFile, name: &str) -> u32 {
    ir.functions
        .iter()
        .position(|function| function.name == name)
        .expect("the function is lowered") as u32
}

const DECLINED: &str = "an annotation instance (`Tag`)";

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

#[test]
fn a_constructed_annotation_instance_is_declined() {
    let ir = lower(
        "annotation class Tag(val n: Int)\n\
         fun box(): String = if (Tag(1) == Tag(1)) \"OK\" else \"fail\"\n",
    );
    assert_eq!(
        check_supported(&ir, annotation(&ir, "Tag")),
        Err(DECLINED.to_string())
    );
}

/// A construction only in a cloning-only `@InlineOnly` template, or in an expression no body owns,
/// is never emitted, so a program that otherwise only applies the annotation is laid out. The same
/// IR with the construction's function emitted still declines.
#[test]
fn an_unemitted_construction_is_no_obligation() {
    let mut ir = lower(
        "annotation class Tag(val n: Int)\n\
         fun template(): Tag = Tag(1)\n\
         @Tag(2) fun box(): String = \"OK\"\n",
    );
    assert_eq!(
        constructions(&ir, "Tag"),
        1,
        "the template holds the construction"
    );
    let tag = annotation(&ir, "Tag");
    assert_eq!(check_supported(&ir, tag), Err(DECLINED.to_string()));

    let template = function(&ir, "template");
    ir.inline_fns.insert(template);
    ir.inline_only_fns.insert(template);
    assert_eq!(check_supported(&ir, tag), Ok(()), "an inline-only template");

    ir.inline_fns.remove(&template);
    ir.inline_only_fns.remove(&template);
    ir.functions[template as usize].body = None;
    assert_eq!(
        constructions(&ir, "Tag"),
        1,
        "the construction is now an orphan"
    );
    assert_eq!(check_supported(&ir, tag), Ok(()), "an orphan expression");
}
