//! JVM realization of checked accesses to properties declared in the active source file.
//!
//! Common lowering materializes declaration storage/accessor candidates, but keeps every ordinary
//! read/write as a stable checked property operation. This pass is the first place that chooses a
//! JVM field access versus an accessor invocation.

use crate::fir::PropertyId;
use crate::ir::{Callee, ExprId, IrCheckedOperation, IrExpr, IrFile, IrLocalPropertyLayout};
use crate::types::{Ty, TypeName};

pub(super) fn realize(ir: &mut IrFile) -> Result<(), PropertyId> {
    for raw in 0..ir.exprs.len() {
        let replacement = match ir.exprs[raw].clone() {
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                substitutions: _,
            }) => {
                let Some(layout) = ir.local_property_layouts.get(&target) else {
                    continue;
                };
                Some(property_read(
                    ir,
                    layout,
                    dispatch_receiver,
                    extension_receiver,
                    &context_arguments,
                    target,
                )?)
            }
            IrExpr::Checked(IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                substitutions: _,
            }) => {
                let Some(layout) = ir.local_property_layouts.get(&target) else {
                    continue;
                };
                Some(property_write(
                    ir,
                    layout,
                    dispatch_receiver,
                    extension_receiver,
                    &context_arguments,
                    value,
                    target,
                )?)
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            ir.exprs[raw] = replacement;
        }
    }
    Ok(())
}

fn property_read(
    ir: &IrFile,
    layout: &IrLocalPropertyLayout,
    dispatch_receiver: Option<ExprId>,
    extension_receiver: Option<ExprId>,
    context_arguments: &[ExprId],
    target: PropertyId,
) -> Result<IrExpr, PropertyId> {
    match layout {
        IrLocalPropertyLayout::TopLevelStorage {
            storage,
            getter,
            qualifier,
            ..
        } => {
            if !static_dispatch_matches(ir, *qualifier, dispatch_receiver)
                || extension_receiver.is_some()
                || !context_arguments.is_empty()
            {
                Err(target)
            } else if let Some(getter) = getter {
                Ok(IrExpr::Call {
                    callee: Callee::Local(*getter),
                    dispatch_receiver: None,
                    args: Vec::new(),
                })
            } else {
                Ok(IrExpr::GetStatic(*storage))
            }
        }
        IrLocalPropertyLayout::TopLevelAccessor {
            getter,
            receiver,
            context_parameters,
            ..
        } => {
            if context_arguments.len() != context_parameters.len() {
                return Err(target);
            }
            let mut args = context_arguments.to_vec();
            match (receiver, dispatch_receiver, extension_receiver) {
                (None, None, None) => {}
                (Some(_), None, Some(receiver)) => args.push(receiver),
                _ => return Err(target),
            }
            Ok(IrExpr::Call {
                callee: Callee::Local(*getter),
                dispatch_receiver: None,
                args,
            })
        }
        IrLocalPropertyLayout::MemberExtension {
            owner,
            interface,
            name,
            receiver,
            ty,
            context_parameters,
            ..
        } => {
            if context_arguments.len() != context_parameters.len() {
                return Err(target);
            }
            let dispatch = dispatch_receiver.ok_or(target)?;
            let extension = extension_receiver.ok_or(target)?;
            let mut args = context_arguments.to_vec();
            args.push(extension);
            Ok(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: *owner,
                    name: crate::names::property_getter_name(name),
                    descriptor: String::new(),
                    params: Some((
                        context_parameters
                            .iter()
                            .copied()
                            .chain(std::iter::once(*receiver))
                            .collect(),
                        *ty,
                    )),
                    interface: *interface,
                },
                dispatch_receiver: Some(dispatch),
                args,
            })
        }
        IrLocalPropertyLayout::Member {
            class,
            owner,
            backing_field,
            getter,
            interface,
            name,
            ty,
            private,
            context_parameters,
            property,
            ..
        } => {
            if context_arguments.len() != context_parameters.len() {
                return Err(target);
            }
            let receiver = dispatch_receiver.ok_or(target)?;
            let class_declaration = &ir.classes[*class as usize];
            let direct_field = class_declaration.properties[*property as usize]
                .annotations
                .iter()
                .any(|annotation| annotation.matches("kotlin/jvm/JvmField"));
            if direct_field {
                let field = backing_field.ok_or(target)?;
                if !context_arguments.is_empty() {
                    return Err(target);
                }
                return Ok(IrExpr::GetField {
                    receiver,
                    class: *class,
                    index: field,
                });
            }
            if *private {
                if let Some(field) = backing_field {
                    if !context_arguments.is_empty() {
                        return Err(target);
                    }
                    return Ok(IrExpr::GetField {
                        receiver,
                        class: *class,
                        index: *field,
                    });
                }
                getter.ok_or(target)?;
            }
            Ok(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: *owner,
                    name: if class_declaration.is_annotation {
                        name.clone()
                    } else {
                        crate::names::property_getter_name(name)
                    },
                    descriptor: String::new(),
                    params: Some((context_parameters.clone(), *ty)),
                    interface: *interface
                        || class_declaration.is_interface
                        || class_declaration.is_annotation,
                },
                dispatch_receiver: Some(receiver),
                args: context_arguments.to_vec(),
            })
        }
    }
}

fn property_write(
    ir: &IrFile,
    layout: &IrLocalPropertyLayout,
    dispatch_receiver: Option<ExprId>,
    extension_receiver: Option<ExprId>,
    context_arguments: &[ExprId],
    value: ExprId,
    target: PropertyId,
) -> Result<IrExpr, PropertyId> {
    match layout {
        IrLocalPropertyLayout::TopLevelStorage {
            storage,
            setter,
            qualifier,
            ..
        } => {
            if !static_dispatch_matches(ir, *qualifier, dispatch_receiver)
                || extension_receiver.is_some()
                || !context_arguments.is_empty()
            {
                Err(target)
            } else if let Some(setter) = setter {
                Ok(IrExpr::Call {
                    callee: Callee::Local(*setter),
                    dispatch_receiver: None,
                    args: vec![value],
                })
            } else {
                Ok(IrExpr::SetStatic {
                    index: *storage,
                    value,
                })
            }
        }
        IrLocalPropertyLayout::TopLevelAccessor {
            setter,
            receiver,
            context_parameters,
            ..
        } => {
            let setter = setter.ok_or(target)?;
            if context_arguments.len() != context_parameters.len() {
                return Err(target);
            }
            let mut args = context_arguments.to_vec();
            match (receiver, dispatch_receiver, extension_receiver) {
                (None, None, None) => {}
                (Some(_), None, Some(receiver)) => args.push(receiver),
                _ => return Err(target),
            }
            args.push(value);
            Ok(IrExpr::Call {
                callee: Callee::Local(setter),
                dispatch_receiver: None,
                args,
            })
        }
        IrLocalPropertyLayout::Member {
            class,
            owner,
            backing_field,
            setter,
            interface,
            name,
            ty,
            mutable,
            private,
            context_parameters,
            property,
            ..
        } => {
            if context_arguments.len() != context_parameters.len() || !mutable {
                return Err(target);
            }
            let receiver = dispatch_receiver.ok_or(target)?;
            let direct_field = ir.classes[*class as usize].properties[*property as usize]
                .annotations
                .iter()
                .any(|annotation| annotation.matches("kotlin/jvm/JvmField"));
            if direct_field {
                let field = backing_field.ok_or(target)?;
                if !context_arguments.is_empty() {
                    return Err(target);
                }
                return Ok(IrExpr::SetField {
                    receiver,
                    class: *class,
                    index: field,
                    value,
                });
            }
            if *private {
                if let Some(field) = backing_field {
                    if !context_arguments.is_empty() {
                        return Err(target);
                    }
                    return Ok(IrExpr::SetField {
                        receiver,
                        class: *class,
                        index: *field,
                        value,
                    });
                }
                setter.ok_or(target)?;
            }
            Ok(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: *owner,
                    name: crate::names::property_setter_name(name),
                    descriptor: String::new(),
                    params: Some((
                        context_parameters
                            .iter()
                            .copied()
                            .chain(std::iter::once(*ty))
                            .collect(),
                        Ty::Unit,
                    )),
                    interface: *interface,
                },
                dispatch_receiver: Some(receiver),
                args: context_arguments
                    .iter()
                    .copied()
                    .chain(std::iter::once(value))
                    .collect(),
            })
        }
        IrLocalPropertyLayout::MemberExtension {
            owner,
            interface,
            name,
            setter,
            receiver,
            ty,
            context_parameters,
        } => {
            if context_arguments.len() != context_parameters.len() || setter.is_none() {
                return Err(target);
            }
            let dispatch = dispatch_receiver.ok_or(target)?;
            let extension = extension_receiver.ok_or(target)?;
            Ok(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: *owner,
                    name: crate::names::property_setter_name(name),
                    descriptor: String::new(),
                    params: Some((
                        context_parameters
                            .iter()
                            .copied()
                            .chain([*receiver, *ty])
                            .collect(),
                        Ty::Unit,
                    )),
                    interface: *interface,
                },
                dispatch_receiver: Some(dispatch),
                args: context_arguments
                    .iter()
                    .copied()
                    .chain([extension, value])
                    .collect(),
            })
        }
    }
}

fn static_dispatch_matches(
    ir: &IrFile,
    qualifier: Option<TypeName>,
    dispatch: Option<ExprId>,
) -> bool {
    match (qualifier, dispatch) {
        (None, None) => true,
        (Some(expected), Some(dispatch)) => matches!(
            ir.exprs.get(dispatch as usize),
            Some(IrExpr::SingletonValue { classifier }) if *classifier == expected
        ),
        (None, Some(_)) | (Some(_), None) => false,
    }
}
