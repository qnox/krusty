//! Expansion of a checked call of an inline dependency function from its template.
//!
//! The call's operands are laid out as for a same-module inline function, in the template's
//! parameter order: a member's dispatch receiver, context arguments, the extension receiver, then
//! the value arguments, with each lambda literal kept for splicing. The template then expands as a
//! same-module inline body does. A call the template cannot expand is not lowered at all: a selected template is an
//! obligation, never retried as a call of the dependency function.

use super::super::inlining::InlineCallee;
use super::super::BodyLowering;
use super::super::DependencyInlineTemplate;
use super::{argument_boundaries, CheckedArgumentPolicy};
use crate::fir::{ExternalCallableId, FirTypeSubstitution};
use crate::ir::{ExprId, IrCheckedArgument, IrExpr};
use crate::types::Ty;

/// A checked call of an inline dependency function that has a template in this file.
pub(in super::super) struct DependencyInlineCall<'a> {
    pub(in super::super) target: ExternalCallableId,
    pub(in super::super) template: DependencyInlineTemplate,
    pub(in super::super) parameter_types: &'a [Ty],
    pub(in super::super) dispatch_receiver: Option<ExprId>,
    /// The checked dispatch receiver applied as the member's declaring class, whose arguments
    /// bind a generic class's parameters in the template.
    pub(in super::super) dispatch_type: Option<Ty>,
    pub(in super::super) extension_receiver: Option<ExprId>,
    /// The slot a member extension's checked call gives its extension receiver among
    /// `parameter_types`; its arguments are numbered around that slot.
    pub(in super::super) extension_receiver_parameter: Option<u32>,
    pub(in super::super) arguments: &'a [IrCheckedArgument],
    pub(in super::super) substitutions: &'a [FirTypeSubstitution],
}

impl BodyLowering<'_> {
    pub(in super::super) fn dependency_inline_call(
        &mut self,
        call: DependencyInlineCall<'_>,
    ) -> Option<ExprId> {
        let DependencyInlineCall {
            target,
            template,
            parameter_types,
            dispatch_receiver,
            dispatch_type,
            extension_receiver,
            extension_receiver_parameter,
            arguments,
            substitutions,
        } = call;
        // A member extension's call carries its extension receiver as a checked parameter slot.
        // The receiver is laid out here as any extension receiver is, so the slot is taken out and
        // the arguments numbered as the declaration's value parameters.
        let (parameter_types, arguments) = match extension_receiver_parameter {
            None => (parameter_types.to_vec(), arguments.to_vec()),
            Some(slot) => {
                let mut types = parameter_types.to_vec();
                if slot as usize >= types.len() {
                    return None;
                }
                types.remove(slot as usize);
                let arguments = arguments
                    .iter()
                    .map(|argument| {
                        let mut argument = argument.clone();
                        let parameter = match &mut argument {
                            IrCheckedArgument::Expression { parameter, .. }
                            | IrCheckedArgument::Default { parameter }
                            | IrCheckedArgument::Vararg { parameter, .. } => parameter,
                        };
                        match (*parameter).cmp(&slot) {
                            std::cmp::Ordering::Less => {}
                            std::cmp::Ordering::Equal => return None,
                            std::cmp::Ordering::Greater => *parameter -= 1,
                        }
                        Some(argument)
                    })
                    .collect::<Option<Vec<_>>>()?;
                (types, arguments)
            }
        };
        let (parameter_types, arguments) = (parameter_types.as_slice(), arguments.as_slice());
        let function = template.function;
        let declared = self.ir.functions.get(function as usize)?.params.clone();
        let context_count = self.ir.declared_parameters(function).context_count;
        let receiver_position = self
            .ir
            .extension_receiver_fns
            .contains(&function)
            .then_some(context_count);
        if receiver_position.is_some() != extension_receiver.is_some() {
            return None;
        }
        let dispatch_class = self.ir.functions.get(function as usize)?.dispatch_receiver;
        if dispatch_class.is_some() != dispatch_receiver.is_some() {
            return None;
        }
        let mut declared_values = declared.clone();
        if let Some(position) = receiver_position {
            declared_values.remove(position);
        }
        if declared_values.len() != parameter_types.len() {
            return None;
        }
        let class_arguments = match (dispatch_class, template.class_type_parameters.is_empty()) {
            (_, true) => Vec::new(),
            (Some(classifier), false) => match dispatch_type?.kind() {
                crate::types::TyKind::Obj(owner, arguments) if owner == classifier => {
                    arguments.to_vec()
                }
                _ => return None,
            },
            (None, false) => return None,
        };
        let callee = InlineCallee::Dependency {
            target,
            template,
            class_arguments,
        };
        let (bindings, _) = callee.substitution_bindings(substitutions, |_| None)?;
        let direct = argument_boundaries::follow_parameter_order(arguments, None);
        let mut statements = Vec::new();
        // The dispatch receiver is evaluated first, as the member call it expands would.
        let dispatch = match (dispatch_receiver, dispatch_class) {
            (Some(receiver), Some(classifier)) => {
                let declared = Ty::obj_name(classifier);
                Some(self.inline_receiver_operand(
                    receiver,
                    declared,
                    declared,
                    direct,
                    &mut statements,
                ))
            }
            _ => None,
        };
        let receiver = match (extension_receiver, receiver_position) {
            (Some(receiver), Some(position)) => {
                let declared = declared[position];
                let specialized = crate::types::ty_subst_keep_unbound(declared, &bindings);
                Some(self.inline_receiver_operand(
                    receiver,
                    declared,
                    specialized,
                    direct,
                    &mut statements,
                ))
            }
            _ => None,
        };
        let normalized = self.normalize_checked_arguments(
            parameter_types,
            arguments,
            CheckedArgumentPolicy::SameFileInline {
                declared_parameters: &declared_values,
                inline: true,
            },
            direct,
            true,
        )?;
        statements.extend(normalized.statements);
        let mut operands = normalized.slots;
        let mut inline_lambdas = normalized.inline_lambdas;
        if let (Some(receiver), Some(position)) = (receiver, receiver_position) {
            operands.insert(position, Some(receiver));
            inline_lambdas.insert(position, None);
        }
        if let Some(dispatch) = dispatch {
            operands.insert(0, Some(dispatch));
            inline_lambdas.insert(0, None);
        }
        let mut malformed = None;
        let inlined = self.inline_same_file_call(
            &callee,
            function,
            &operands,
            &inline_lambdas,
            substitutions,
            &mut malformed,
        )?;
        let expanded = if statements.is_empty() {
            inlined
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(inlined),
            })
        };
        self.ir.inline_regions.insert(expanded);
        self.ir.external_inline_expansions.insert(expanded);
        Some(expanded)
    }
}

impl BodyLowering<'_> {
    /// The expansion of the library declaration a range construction selected, for a target that
    /// does not lay its range class out: kotlinc expands that inline declaration at the
    /// construction (`UInt.rangeTo`). `None` when the construction stays a range construction.
    pub(in super::super) fn library_range_expansion(
        &mut self,
        (ends_inlined_call, declaration): (bool, Option<ExternalCallableId>),
        [start, end]: [ExprId; 2],
        [start_type, end_type]: [Ty; 2],
        result: Ty,
    ) -> Result<Option<ExprId>, super::super::FirLoweringFailure> {
        let Some(target) = declaration.filter(|_| ends_inlined_call) else {
            return Ok(None);
        };
        let builds_by_library = result
            .kotlin_class_internal()
            .is_some_and(|class| self.ir.builds_range_by_library(class));
        if !builds_by_library
            || self.ir.same_module_inlining() != crate::ir::SameModuleInlining::ExpandInIr
            || self.dependency_templates.compiler_implemented(target)
        {
            return Ok(None);
        }
        let Some(template) = self.dependency_templates.template(self.ir, target).cloned() else {
            return Ok(None);
        };
        let is_extension = self.ir.extension_receiver_fns.contains(&template.function);
        let arguments = [IrCheckedArgument::Expression {
            parameter: 0,
            value: end,
            context: crate::ir::IrContextArgumentRole::Ordinary,
        }];
        // A selected template is an obligation: a construction it cannot expand fails the file.
        self.dependency_inline_call(DependencyInlineCall {
            target,
            template,
            parameter_types: &[end_type],
            dispatch_receiver: (!is_extension).then_some(start),
            dispatch_type: Some(start_type),
            extension_receiver: is_extension.then_some(start),
            extension_receiver_parameter: None,
            arguments: &arguments,
            substitutions: &[],
        })
        .map(Some)
        .ok_or(super::super::FirLoweringFailure::UnsupportedExternalCall(
            target,
        ))
    }
}
