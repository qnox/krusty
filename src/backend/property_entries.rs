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
        match ir.local_property_layouts.get(&property).cloned() {
            Some(IrLocalPropertyLayout::Member {
                class,
                owner,
                backing_field: Some(_),
                getter,
                setter: declared_setter,
                context_parameters,
                mutable,
                ..
            }) => {
                if (setter && (!mutable || declared_setter.is_some()))
                    || (!setter && getter.is_some())
                {
                    return Err("a default property export disagreeing with its checked accessor");
                }
                let declaration = ir
                    .checked_properties
                    .get(&property)
                    .ok_or("a default property export without its checked declaration")?;
                if declaration.class != Some(class) {
                    return Err("a default property export with a different declaration owner");
                }
                let ty = declaration.ty;
                let receiver_type = ir
                    .declared_class_self_type(owner)
                    .ok_or("a default property export without its declared receiver application")?;
                let receiver = read(ir, 0, receiver_type);
                let contexts = context_parameters
                    .iter()
                    .enumerate()
                    .map(|(index, &ty)| read(ir, index as u32 + 1, ty))
                    .collect::<Vec<_>>();
                let entry = Entry {
                    property,
                    setter,
                    ty,
                    receiver: Some(receiver),
                    contexts,
                    parameters: context_parameters,
                    name: if setter {
                        "<property>$set"
                    } else {
                        "<property>$get"
                    }
                    .to_owned(),
                    owner: Some(owner),
                };
                let function = entry.realize(ir);
                exports.record_callable(function, identity);
                exports.record_member_receiver(function, receiver_type);
            }
            Some(IrLocalPropertyLayout::TopLevelStorage {
                getter,
                setter: declared_setter,
                qualifier: None,
                ..
            }) => {
                let declaration = ir
                    .package_properties
                    .iter()
                    .find(|declaration| declaration.property == property)
                    .ok_or("a default property export without its package declaration")?;
                if (setter && (!declaration.mutable || declared_setter.is_some()))
                    || (!setter && getter.is_some())
                {
                    return Err("a default property export disagreeing with its checked accessor");
                }
                // Kotlin's accessor names, which keep two properties of one package distinct.
                let name = format!(
                    "<{}-{}>",
                    if setter { "set" } else { "get" },
                    declaration.name
                );
                let entry = Entry {
                    property,
                    setter,
                    ty: declaration.ty,
                    receiver: None,
                    contexts: Vec::new(),
                    parameters: Vec::new(),
                    name,
                    owner: None,
                };
                let function = entry.realize(ir);
                exports.record_callable(function, identity);
            }
            _ => return Err("a default property export without checked storage"),
        }
    }
    Ok(())
}

/// One default accessor body: the checked storage operation over its receiver and context
/// parameters, and, for a setter, the value parameter after them.
struct Entry {
    property: crate::fir::PropertyId,
    setter: bool,
    ty: Ty,
    receiver: Option<ExprId>,
    contexts: Vec<ExprId>,
    parameters: Vec<Ty>,
    name: String,
    owner: Option<crate::types::TypeName>,
}

impl Entry {
    fn realize(self, ir: &mut IrFile) -> crate::ir::FunId {
        let Self {
            property,
            setter,
            ty,
            receiver,
            contexts,
            mut parameters,
            name,
            owner,
        } = self;
        let (operation, result) = if setter {
            let slot = parameters.len() as u32 + u32::from(receiver.is_some());
            let value = read(ir, slot, ty);
            parameters.push(ty);
            (
                IrCheckedOperation::PropertyWrite {
                    target: property,
                    dispatch_receiver: receiver,
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
                    dispatch_receiver: receiver,
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
            name,
            params: parameters,
            ret: result,
            body: Some(body),
            is_static: owner.is_none(),
            dispatch_receiver: owner,
            param_checks: Vec::new(),
        });
        ir.synthetic_methods.insert(function);
        function
    }
}

fn read(ir: &mut IrFile, slot: u32, ty: Ty) -> ExprId {
    let value = ir.add_expr(IrExpr::GetValue(slot));
    ir.set_logical_type(value, ty);
    value
}
