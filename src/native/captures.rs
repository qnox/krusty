//! Shared cells for the mutable locals a local or anonymous class captures.
//!
//! `var a = 1; object { init { a = 2 } }` is one variable, not two: the object's write has to be
//! the enclosing function's read. So the variable is not copied into the object — it is moved into
//! a one-slot heap cell that both sides point at. The checked lowering already does the moving: the
//! declaration becomes an `IrExpr::RefNew` and every use an `IrExpr::RefGet`/`IrExpr::RefSet`,
//! which the generator lowers like any other object (`codegen::lower::functions`).
//!
//! What common IR deliberately does NOT do is say what the cell looks like. It keeps the field's
//! SEMANTIC type — `Int` for `var a: Int` — and marks the coordinate in
//! `IrFile::shared_class_capture_fields` instead, so neither the JVM's `Ref$IntRef` nor this
//! backend's choice leaks into the frontend. This module makes the choice for the native backend,
//! at the coordinate and nowhere else: a marked field, and the constructor argument that fills it,
//! carry a plain reference, because a reference to the cell is what the enclosing function holds.

use crate::ir::{ClassId, IrExpr, IrFile};
use crate::types::Ty;

/// Does this class slot carry a shared cell rather than the value it was declared with?
pub(crate) fn is_shared(ir: &IrFile, class: ClassId, index: u32) -> bool {
    ir.shared_class_capture_fields.contains_key(&(class, index))
}

/// The PHYSICAL type of a class slot — a field, or the constructor argument that fills it — given
/// the type the declaration carries. A shared cell is an ordinary heap reference: its own type
/// descriptor (`codegen::lower::functions::holder_type`) records where the element sits, so the
/// collector traces through it without the field having to say more than "a reference".
pub(crate) fn physical_ty(ir: &IrFile, class: ClassId, index: u32, declared: Ty) -> Ty {
    if is_shared(ir, class, index) {
        Ty::obj("kotlin/Any")
    } else {
        declared
    }
}

/// The types a function's parameters are CARRIED as, which are not always the types it declares.
///
/// A `var` that a closure captures is replaced by a holder, and what is passed to the lambda's body
/// is that cell — but the parameter still says `Int`, because `Int` is what the programmer wrote.
/// Believing the declaration instead truncates a pointer into a 32-bit parameter, which is a
/// miscompile with no symptom where it happens.
///
/// Common lowering RECORDS which parameters carry a holder, in `shared_capture_parameters`, and
/// that record is the answer. The body scan below is a second, weaker source for the same fact —
/// a body that reaches a holder through [`IrExpr::RefGet`]/[`IrExpr::RefSet`] is holding one. It
/// cannot see a parameter the body only PASSES ON: a lambda that does nothing with the cell but
/// hand it to an object it constructs dereferences it nowhere, so the scan alone typed that
/// parameter `Int` and the thunk loaded a pointer as an `i32`. Both are consulted because the
/// record is authoritative and the scan costs nothing; neither can wrongly claim a parameter is a
/// holder, only miss one.
pub(crate) fn carried_parameters(ir: &IrFile, id: crate::ir::FunId) -> Vec<Ty> {
    let function = &ir.functions[id as usize];
    let Some(body) = function.body else {
        return function.params.clone();
    };
    // `this` occupies slot 0 when there is one, so a parameter's slot is offset by it.
    let first = usize::from(function.dispatch_receiver.is_some());
    let mut holders = vec![false; function.params.len() + first];
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let IrExpr::RefGet { holder, .. } | IrExpr::RefSet { holder, .. } = ir.expr(id) {
            if let IrExpr::GetValue(slot) = ir.expr(*holder) {
                if let Some(flag) = holders.get_mut(*slot as usize) {
                    *flag = true;
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, id, &mut |child| pending.push(child));
    }
    function
        .params
        .iter()
        .enumerate()
        .map(|(index, declared)| {
            let ordinal = u32::try_from(index).expect("too many parameters");
            if holders[index + first] || ir.shared_capture_parameters.contains_key(&(id, ordinal)) {
                Ty::nullable(Ty::obj("kotlin/Any"))
            } else {
                *declared
            }
        })
        .collect()
}
