//! The bodies of the representation bridges a class table holds (see `Slot::Bridge`): a function
//! of the overridden member's signature that converts each operand to the override's carrier,
//! forwards through the override's own slot, and converts the result back.
//!
//! Forwarding by dispatch rather than by calling the override is what keeps a further override
//! reachable through the base: that subclass replaces the target slot, not the bridge.

use crate::types::Ty;

use super::super::super::super::encode::{Function, HeapType, Module};
use super::super::super::super::objects::slot_field;
use super::super::super::super::runtime::Runtime;
use super::super::super::classes::{FileClasses, OverrideBridge};
use super::{Body, FileContext, Unsupported};

pub(super) fn define_override_bridges(
    context: &FileContext<'_>,
    classes: &FileClasses,
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<(), Unsupported> {
    let ir = context.ir;
    for &(bridge, function, ty) in &classes.bridges {
        let OverrideBridge {
            class,
            declared,
            target_slot,
            target,
        } = bridge;
        let base = &ir.functions[declared as usize];
        let implementation = &ir.functions[target as usize];
        if base.params.len() != implementation.params.len() {
            return Err(format!(
                "an override bridge whose operands disagree with the member it overrides (`{}`)",
                implementation.name
            ));
        }
        let mut parameters = vec![Ty::obj_name(ir.classes[class as usize].fq_name)];
        parameters.extend(base.params.iter().copied());
        let mut body = Body::new(
            context,
            module,
            runtime,
            &parameters,
            crate::fx_hash::HashMap::default(),
            base.ret,
        );
        body.code.local_get(0);
        for (index, (from, to)) in base.params.iter().zip(&implementation.params).enumerate() {
            body.code.local_get(index as u32 + 1);
            body.coerce_emitted_value(*from, *to)?;
        }
        match target_slot {
            Some(slot) => {
                let table_type = classes.forms[class as usize]
                    .table_type
                    .ok_or("an override bridge in a class with no table type")?;
                let target_type = classes
                    .slot_type(class, slot)
                    .ok_or("an override bridge forwarding through an untyped slot")?;
                body.code.local_get(0);
                body.table_of_object();
                body.code
                    .ref_cast(false, HeapType::Concrete(table_type))
                    .struct_get(table_type, slot_field(slot))
                    .call_ref(target_type);
            }
            None => {
                let direct = context
                    .functions
                    .get(target as usize)
                    .copied()
                    .flatten()
                    .ok_or("an override bridge to a function with no emitted body")?;
                body.code.call(direct);
            }
        }
        body.coerce_emitted_value(implementation.ret, base.ret)?;
        let (code, locals) = body.finish();
        module.define(function, Function { ty, locals, code });
    }
    Ok(())
}
