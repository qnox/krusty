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

use crate::ir::{ClassId, IrFile};
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
