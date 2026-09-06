//! Exact realization of checked accesses to properties declared in the current source module.
//!
//! Pass 2 has already selected a stable [`PropertyId`]. This module joins that identity to the
//! declaration layout produced in common IR. It performs no name lookup and makes no target ABI
//! choice: member accesses remain semantic [`IrExpr::PropertyRead`] / [`IrExpr::PropertyWrite`]
//! nodes for the selected backend to realize.

use crate::fir::PropertyId;
use crate::ir::{Callee, ExprId, IrCheckedOperation, IrExpr, IrFile, IrLocalPropertyLayout};
use crate::types::{Ty, TypeName};

#[derive(Clone, Copy, Debug)]
pub(crate) struct RealizedLocalPropertyAccess {
    pub operation: ExprId,
    pub target: PropertyId,
    pub read: bool,
    pub direct_member: bool,
}

struct CheckedPropertyAccess<'a> {
    dispatch_receiver: Option<ExprId>,
    extension_receiver: Option<ExprId>,
    context_arguments: &'a [ExprId],
    target: PropertyId,
    operation: ExprId,
}

pub(crate) fn realize(ir: &mut IrFile) -> Result<Vec<RealizedLocalPropertyAccess>, PropertyId> {
    let mut realized = Vec::new();
    for raw in 0..ir.exprs.len() {
        let operation = raw as ExprId;
        let (replacement, target, read, layout) = match ir.exprs[raw].clone() {
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                substitutions: _,
            }) => {
                let Some(layout) = ir.local_property_layouts.get(&target).cloned() else {
                    continue;
                };
                let replacement = property_read(
                    ir,
                    &layout,
                    CheckedPropertyAccess {
                        dispatch_receiver,
                        extension_receiver,
                        context_arguments: &context_arguments,
                        target,
                        operation,
                    },
                )?;
                (replacement, target, true, layout)
            }
            IrExpr::Checked(IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                substitutions: _,
            }) => {
                let Some(layout) = ir.local_property_layouts.get(&target).cloned() else {
                    continue;
                };
                let replacement = property_write(
                    ir,
                    &layout,
                    CheckedPropertyAccess {
                        dispatch_receiver,
                        extension_receiver,
                        context_arguments: &context_arguments,
                        target,
                        operation,
                    },
                    value,
                )?;
                (replacement, target, false, layout)
            }
            _ => continue,
        };
        let direct_member = matches!(
            layout,
            IrLocalPropertyLayout::Member {
                ref context_parameters,
                ..
            } if context_parameters.is_empty()
        );
        ir.exprs[raw] = replacement;
        realized.push(RealizedLocalPropertyAccess {
            operation,
            target,
            read,
            direct_member,
        });
    }
    Ok(realized)
}

fn property_read(
    ir: &IrFile,
    layout: &IrLocalPropertyLayout,
    access: CheckedPropertyAccess<'_>,
) -> Result<IrExpr, PropertyId> {
    let CheckedPropertyAccess {
        dispatch_receiver,
        extension_receiver,
        context_arguments,
        target,
        operation,
    } = access;
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
            getter,
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
                    name: ir
                        .functions
                        .get(*getter as usize)
                        .map(|function| function.name.clone())
                        .ok_or(target)?,
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
            interface,
            name,
            getter,
            ty,
            context_parameters,
            ..
        } => {
            if context_arguments.len() != context_parameters.len() || extension_receiver.is_some() {
                return Err(target);
            }
            let receiver = dispatch_receiver.ok_or(target)?;
            if context_arguments.is_empty() {
                return Ok(IrExpr::PropertyRead {
                    receiver: Some(receiver),
                    owner: *owner,
                    name: name.clone(),
                    ty: *ty,
                    interface: *interface,
                    operation: Some(operation),
                });
            }
            let class_declaration = &ir.classes[*class as usize];
            Ok(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: *owner,
                    name: getter
                        .and_then(|getter| ir.functions.get(getter as usize))
                        .map(|function| function.name.clone())
                        .ok_or(target)?,
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
    access: CheckedPropertyAccess<'_>,
    value: ExprId,
) -> Result<IrExpr, PropertyId> {
    let CheckedPropertyAccess {
        dispatch_receiver,
        extension_receiver,
        context_arguments,
        target,
        operation,
    } = access;
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
            owner,
            interface,
            name,
            setter,
            ty,
            mutable,
            context_parameters,
            ..
        } => {
            if context_arguments.len() != context_parameters.len()
                || !mutable
                || extension_receiver.is_some()
            {
                return Err(target);
            }
            let receiver = dispatch_receiver.ok_or(target)?;
            if context_arguments.is_empty() {
                return Ok(IrExpr::PropertyWrite {
                    receiver: Some(receiver),
                    owner: *owner,
                    name: name.clone(),
                    value,
                    ty: *ty,
                    interface: *interface,
                    operation: Some(operation),
                });
            }
            Ok(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: *owner,
                    name: setter
                        .and_then(|setter| ir.functions.get(setter as usize))
                        .map(|function| function.name.clone())
                        .ok_or(target)?,
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
            name: _,
            getter: _,
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
                    name: setter
                        .and_then(|setter| ir.functions.get(setter as usize))
                        .map(|function| function.name.clone())
                        .ok_or(target)?,
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
