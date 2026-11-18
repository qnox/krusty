//! [`crate::backend::property_reference_values::realize`] over programs lowered from source: only a
//! reference an emitted body reaches becomes a function value with its adapters. A reference that
//! stays in the arena as an orphan, or inside a retained inline template, is never executed, so it
//! introduces no adapter body and none of that body's dependency roots.

use crate::ir::{ExprId, IrCheckedOperation, IrExpr, IrFile};

const PROGRAM: &str = "\
var p = 0
inline fun template(): kotlin.reflect.KMutableProperty0<Int> = ::p
fun box(): String {
    val r = ::p
    r.set(1)
    return if (p == 1) \"OK\" else \"fail\"
}
";

/// The program with `template` a retained inline-only template, as the inliner leaves a body it
/// keeps only to clone.
fn lowered() -> IrFile {
    let mut ir = super::lower_single_source_with_jvm_stdlib(PROGRAM, "Reference");
    let template = ir
        .functions
        .iter()
        .position(|function| function.name == "template")
        .expect("the inline function is lowered") as u32;
    ir.inline_only_fns.insert(template);
    ir
}

fn checked_references(ir: &IrFile) -> Vec<ExprId> {
    (0..ir.exprs.len() as ExprId)
        .filter(|&id| {
            matches!(
                ir.expr(id),
                IrExpr::Checked(checked)
                    if matches!(&**checked, IrCheckedOperation::PropertyReference { .. })
            )
        })
        .collect()
}

fn adapters(ir: &IrFile) -> usize {
    ir.functions
        .iter()
        .filter(|function| function.name.starts_with("$property_ref_"))
        .count()
}

#[test]
fn only_an_emitted_reference_is_realized() {
    let mut ir = lowered();
    let before = checked_references(&ir);
    assert_eq!(
        before.len(),
        2,
        "one reference in `box`, one in the template"
    );

    crate::backend::property_reference_values::realize(&mut ir);

    let after = checked_references(&ir);
    assert_eq!(after.len(), 1, "the template's reference stays checked");
    // The emitted reference gets a read adapter and, being mutable, its writer. The template's
    // would add two more.
    assert_eq!(adapters(&ir), 2);
}

#[test]
fn an_orphan_reference_is_not_realized() {
    let mut ir = lowered();
    let reference = checked_references(&ir)[0];
    let orphan = ir.add_expr(ir.expr(reference).clone());

    crate::backend::property_reference_values::realize(&mut ir);

    assert!(
        matches!(ir.expr(orphan), IrExpr::Checked(_)),
        "an expression no emitted body reaches stays as lowered"
    );
    assert_eq!(adapters(&ir), 2, "only `box`'s reference has adapters");
}
