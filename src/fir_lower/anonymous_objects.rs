//! Lowering of checked anonymous-object construction and its synthetic constructor inputs.

use std::collections::HashMap;

use super::{BodyLowering, FirLoweringFailure};
use crate::fir::{FirAnonymousObject, ResolvedInterfaceDelegateSource};
use crate::ir::{ExprId, IrCtorArg, IrExpr};
use crate::types::ContextParameterKind;

impl BodyLowering<'_> {
    pub(super) fn anonymous_object(
        &mut self,
        object: &FirAnonymousObject,
    ) -> Result<ExprId, FirLoweringFailure> {
        let (classifier, mut arguments) =
            self.prepare_captured_class(object.declaration, &object.captures)?;
        let header = self
            .index
            .classifier_header(object.declaration)
            .ok_or(FirLoweringFailure::MissingLocalClass(object.declaration))?;
        let mut delegates = Vec::with_capacity(object.delegate_arguments.len());
        for argument in &object.delegate_arguments {
            let resolved = header
                .interface_delegations
                .get(argument.delegation as usize)
                .ok_or(FirLoweringFailure::MissingLocalClass(object.declaration))?;
            let expected_parameter = arguments
                .len()
                .checked_add(delegates.len())
                .and_then(|parameter| u32::try_from(parameter).ok())
                .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
            if resolved.source
                != ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(
                    expected_parameter,
                )
            {
                return Err(FirLoweringFailure::MissingLocalClass(object.declaration));
            }
            let ty = self
                .body
                .expr(argument.value)
                .ok_or(FirLoweringFailure::MissingExpression(argument.value))?
                .ty
                .get();
            delegates.push((self.expression(argument.value)?, ty));
        }
        let class = self
            .ir
            .checked_classifier_classes
            .get(&object.declaration)
            .copied()
            .ok_or(FirLoweringFailure::MissingLocalClass(object.declaration))?;
        self.ir.classes[class as usize]
            .ctor_args
            .extend(delegates.iter().map(|(_, ty)| IrCtorArg {
                name: None,
                context_kind: ContextParameterKind::None,
                ty: *ty,
                declared_ty: None,
                is_field: false,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: crate::ir::IrCtorParameterProvenance::Value,
                capture_identity: None,
            }));
        let delegate_count = u32::try_from(delegates.len())
            .map_err(|_| FirLoweringFailure::ValueIdentityOverflow)?;
        let class_decl = &mut self.ir.classes[class as usize];
        class_decl.constructor_prefix_count = class_decl
            .constructor_prefix_count
            .checked_add(delegate_count)
            .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
        arguments.extend(delegates);

        let mut forwards = Vec::with_capacity(object.super_arguments.len());
        for argument in &object.super_arguments {
            let ty = self
                .body
                .expr(argument.value)
                .ok_or(FirLoweringFailure::MissingExpression(argument.value))?
                .ty
                .get();
            forwards.push((
                argument.slot,
                argument.type_operator_shells,
                self.expression(argument.value)?,
                ty,
            ));
        }
        let mut prelude = Vec::new();
        let mut temporaries = HashMap::new();
        for argument in &object.super_arguments {
            if temporaries.contains_key(&argument.slot) {
                continue;
            }
            let Some((_, _, value, ty)) = forwards
                .iter()
                .find(|(slot, _, _, _)| *slot == argument.slot)
            else {
                continue;
            };
            let temporary = self.allocate_temporary();
            prelude.push(self.ir.add_expr(IrExpr::Variable {
                index: temporary,
                ty: *ty,
                init: Some(*value),
                named: false,
            }));
            temporaries.insert(argument.slot, temporary);
        }
        forwards.sort_by_key(|(slot, _, _, _)| *slot);
        if !forwards.is_empty() {
            let start = u32::try_from(self.ir.classes[class as usize].ctor_args.len())
                .map_err(|_| FirLoweringFailure::ValueIdentityOverflow)?;
            for (offset, (slot, shells, _, ty)) in forwards.iter().enumerate() {
                let offset =
                    u32::try_from(offset).map_err(|_| FirLoweringFailure::ValueIdentityOverflow)?;
                let parameter = start
                    .checked_add(offset)
                    .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
                let ordinal = offset
                    .checked_add(1)
                    .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
                self.ir
                    .anonymous_super_forwards
                    .insert((object.declaration, *slot), (parameter, *shells));
                self.ir.classes[class as usize].ctor_args.push(IrCtorArg {
                    name: None,
                    context_kind: ContextParameterKind::None,
                    ty: *ty,
                    declared_ty: None,
                    is_field: false,
                    field_index: None,
                    has_default: false,
                    is_vararg: false,
                    type_param: None,
                    check: None,
                    anonymous_super_forward: Some(ordinal),
                    capture: None,
                    provenance: crate::ir::IrCtorParameterProvenance::Value,
                    capture_identity: None,
                });
            }
            let count = u32::try_from(forwards.len())
                .map_err(|_| FirLoweringFailure::ValueIdentityOverflow)?;
            let class_decl = &mut self.ir.classes[class as usize];
            class_decl.constructor_prefix_count = class_decl
                .constructor_prefix_count
                .checked_add(count)
                .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
        }

        let mut lowered_arguments = Vec::with_capacity(arguments.len() + forwards.len());
        let mut parameter_types = Vec::with_capacity(lowered_arguments.capacity());
        for (value, ty) in arguments {
            lowered_arguments.push(value);
            parameter_types.push(ty);
        }
        for (slot, _, _, ty) in &forwards {
            lowered_arguments.push(self.ir.add_expr(IrExpr::GetValue(temporaries[slot])));
            parameter_types.push(*ty);
        }
        let construction = self.ir.add_expr(IrExpr::New {
            internal: classifier,
            args: lowered_arguments,
            ctor_params: Some(parameter_types),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        Ok(if prelude.is_empty() {
            construction
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: prelude,
                value: Some(construction),
            })
        })
    }
}
