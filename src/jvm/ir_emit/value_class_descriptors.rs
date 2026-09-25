//! The value classes a file's code may box, for kotlinc's redundant-boxing pass: each one's
//! internal name and the descriptor its `box-impl` takes (`unboxedTypeOfInlineClass`). A
//! non-null underlying value class is unboxed in turn; a nullable one stays boxed.

use std::collections::{HashMap, HashSet};

use crate::ir::IrFile;
use crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors;
use crate::jvm::names::type_descriptor;
use crate::types::Ty;

use super::ir_ty_to_jvm;

pub(super) fn of(ir: &IrFile) -> ValueClassDescriptors {
    let descriptors: HashMap<String, String> = ir
        .value_class_names()
        .filter_map(|name| {
            let unboxed = unboxed_type(ir, Ty::obj_name(name))?;
            Some((name.render(), type_descriptor(ir_ty_to_jvm(&unboxed))))
        })
        .collect();
    ValueClassDescriptors(descriptors)
}

/// The type a value class's `box-impl` takes; `None` for a cyclic declaration graph.
fn unboxed_type(ir: &IrFile, class: Ty) -> Option<Ty> {
    let mut classifier = class.obj_internal()?;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(classifier) {
            return None;
        }
        let underlying = ir.value_class_underlying_name(classifier)?;
        match underlying.obj_internal().filter(|&next| {
            !underlying.is_nullable() && ir.value_class_underlying_name(next).is_some()
        }) {
            Some(next) => classifier = next,
            None => return Some(underlying),
        }
    }
}
