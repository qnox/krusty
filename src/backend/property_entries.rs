//! Bodies for declaration-owned default property accessors exported across KLIB modules.
//!
//! The metadata owner records the exact property and accessor identity. The body keeps the
//! ordinary checked operation, so each target uses its existing storage, dispatch and carrier
//! realization rather than another implementation of property access.

use super::{DependencyExportSignatures, ExportedPropertyAccessor};
use crate::ir::{ExprId, IrCheckedOperation, IrExpr, IrFile, IrFunction, IrLocalPropertyLayout};
use crate::types::Ty;

pub fn realize(
    ir: &mut IrFile,
    exports: &mut DependencyExportSignatures,
) -> Result<(), &'static str> {
    for (accessor, identity) in exports.property_accessors().to_vec() {
        let (property, setter) = match accessor {
            ExportedPropertyAccessor::Getter(property) => (property, false),
            ExportedPropertyAccessor::Setter(property) => (property, true),
        };
        let Some(layout) = ir.local_property_layouts.get(&property).cloned() else {
            return Err("a default property export without checked storage");
        };
        let (class, owner, getter, declared_setter, context_parameters, mutable) = match layout {
            IrLocalPropertyLayout::Member {
                class,
                owner,
                backing_field: Some(_),
                getter,
                setter,
                context_parameters,
                mutable,
                ..
            } => (
                Some(class),
                Some(owner),
                getter,
                setter,
                context_parameters,
                mutable,
            ),
            IrLocalPropertyLayout::TopLevelStorage {
                storage,
                getter,
                setter: declared_setter,
                qualifier: None,
            } => {
                let Some(storage) = ir.statics.get(storage as usize) else {
                    return Err("a default package property export without checked storage");
                };
                let accessor = if setter {
                    storage.accessors.setter
                } else {
                    storage.accessors.getter
                };
                if storage.owner.is_some()
                    || storage.is_const
                    || storage.property_delegate
                    || accessor != crate::ir::IrStaticAccessor::Default
                {
                    return Err("a default package property export disagreeing with its storage");
                }
                (
                    None,
                    None,
                    getter,
                    declared_setter,
                    Vec::new(),
                    storage.is_var,
                )
            }
            _ => return Err("a default property export without checked storage"),
        };
        if (setter && (!mutable || declared_setter.is_some())) || (!setter && getter.is_some()) {
            return Err("a default property export disagreeing with its checked accessor");
        }
        let declaration = ir
            .checked_properties
            .get(&property)
            .ok_or("a default property export without its checked declaration")?;
        if declaration.class != class {
            return Err("a default property export with a different declaration owner");
        }
        let ty = declaration.ty;
        let (dispatch_receiver, receiver_type) = if let Some(owner) = owner {
            let receiver_type = ir
                .declared_class_self_type(owner)
                .ok_or("a default property export without its declared receiver application")?;
            (Some(read(ir, 0, receiver_type)), Some(receiver_type))
        } else {
            if declaration.class.is_some() {
                return Err("a default package property export with a class declaration");
            }
            (None, None)
        };
        let parameter_start = if dispatch_receiver.is_some() { 1 } else { 0 };
        let contexts = context_parameters
            .iter()
            .enumerate()
            .map(|(index, &ty)| read(ir, parameter_start + index as u32, ty))
            .collect::<Vec<_>>();
        let mut parameters = context_parameters;
        let (operation, result) = if setter {
            let value = read(ir, parameter_start + parameters.len() as u32, ty);
            parameters.push(ty);
            (
                IrCheckedOperation::PropertyWrite {
                    target: property,
                    dispatch_receiver,
                    extension_receiver: None,
                    context_arguments: contexts,
                    value,
                    substitutions: Vec::new(),
                },
                Ty::Unit,
            )
        } else {
            (
                IrCheckedOperation::PropertyRead {
                    target: property,
                    dispatch_receiver,
                    extension_receiver: None,
                    context_arguments: contexts,
                    substitutions: Vec::new(),
                },
                ty,
            )
        };
        let value = ir.add_expr(IrExpr::Checked(Box::new(operation)));
        ir.set_logical_type(value, result);
        let mut statements = Vec::new();
        if setter {
            statements.push(value);
        }
        let returned = ir.add_expr(IrExpr::Return((!setter).then_some(value)));
        statements.push(returned);
        let body = ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        });
        let function = ir.add_fun(IrFunction {
            name: if setter {
                "<property>$set"
            } else {
                "<property>$get"
            }
            .to_string(),
            params: parameters,
            ret: result,
            body: Some(body),
            is_static: owner.is_none(),
            dispatch_receiver: owner,
            param_checks: Vec::new(),
        });
        ir.synthetic_methods.insert(function);
        exports.record_callable(function, identity);
        if let Some(receiver_type) = receiver_type {
            exports.record_member_receiver(function, receiver_type);
        }
    }
    Ok(())
}

fn read(ir: &mut IrFile, slot: u32, ty: Ty) -> ExprId {
    let value = ir.add_expr(IrExpr::GetValue(slot));
    ir.set_logical_type(value, ty);
    value
}
