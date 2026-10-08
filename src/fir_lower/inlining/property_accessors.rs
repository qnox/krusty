//! Expansion of checked inline property accessors.
//!
//! The checker records the accessor declaration and its type arguments on the use. This module
//! clones that accessor's body into the use. It does not look declarations up.

use std::collections::{HashMap, HashSet};

use crate::fir::DeclarationId;
use crate::ir::{
    ExprId, IrCheckedOperation, IrConst, IrDebugLocalProvenance, IrExpr, IrInlineLocalRole,
    IrInlinePropertySplice, IrInlineTypeSubstitution,
};
use crate::types::{stored_value_ty, ty_subst_keep_unbound, Ty};

use super::super::{FirFileLoweringFailure, InlineAccessorFailure as InlineFail};
use super::{
    mark_subtree, produce_sole_tail_return, rebase_values, specialize_inline_copy, value_indices,
    InlineReturn,
};

/// Splice `inline` property accessors at the uses the checker recorded.
///
/// Accessor functions exist by the time this runs. Each use names the accessor declaration and
/// carries that use's type arguments, so a reified `T::class` is the call site's class. The
/// accessor declaration itself stays, erased, for a caller that does not inline it.
pub(super) fn splice_inline_property_accessors(
    ir: &mut crate::ir::IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let mut next_temporary = fresh_value_base(ir);
    let mut sites = ir
        .inline_property_access
        .splices
        .keys()
        .copied()
        .collect::<Vec<_>>();
    // Hash iteration must not decide whether an already-expanded accessor is cloned into a later
    // use. Consumed records are removed; sorting keeps the rest deterministic.
    sites.sort_unstable();
    let mut stack = HashSet::new();
    for site in sites {
        splice_property_access(ir, site, &mut next_temporary, &mut stack)?;
    }
    Ok(())
}

struct PropertyAccess {
    dispatch_receiver: Option<ExprId>,
    extension_receiver: Option<ExprId>,
    context_arguments: Vec<ExprId>,
    /// `Some` for a write: the value being stored.
    value: Option<ExprId>,
}

fn property_access(ir: &crate::ir::IrFile, site: ExprId) -> Option<PropertyAccess> {
    match ir.expr(site).clone() {
        IrExpr::Checked(IrCheckedOperation::PropertyRead {
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            ..
        }) => Some(PropertyAccess {
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            value: None,
        }),
        IrExpr::Checked(IrCheckedOperation::PropertyWrite {
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            value,
            ..
        }) => Some(PropertyAccess {
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            value: Some(value),
        }),
        _ => None,
    }
}

fn splice_property_access(
    ir: &mut crate::ir::IrFile,
    site: ExprId,
    next_temporary: &mut u32,
    stack: &mut HashSet<DeclarationId>,
) -> Result<(), FirFileLoweringFailure> {
    let Some(splice) = ir.inline_property_access.splices.get(&site).cloned() else {
        return Ok(());
    };
    let Some(access) = property_access(ir, site) else {
        return Err(failure(splice.accessor, InlineFail::InvalidSite));
    };
    let function = recorded_accessor(ir, &splice)?;
    if !stack.insert(splice.accessor) {
        return Err(failure(splice.accessor, InlineFail::Recursive));
    }
    let nested = expand_inline_accessor(ir, site, &access, &splice, function, next_temporary)?;
    // The site is now the expanded block. Drop the record so a later clone of this accessor does
    // not try to splice that block again.
    ir.inline_property_access.splices.remove(&site);
    for nested in nested {
        splice_property_access(ir, nested, next_temporary, stack)?;
    }
    stack.remove(&splice.accessor);
    Ok(())
}

fn recorded_accessor(
    ir: &crate::ir::IrFile,
    splice: &IrInlinePropertySplice,
) -> Result<crate::ir::FunId, FirFileLoweringFailure> {
    ir.inline_property_access
        .accessor_functions
        .get(&splice.accessor)
        .copied()
        .ok_or_else(|| failure(splice.accessor, InlineFail::MissingFunction))
}

fn failure(accessor: DeclarationId, reason: InlineFail) -> FirFileLoweringFailure {
    FirFileLoweringFailure::InlineAccessor(accessor, reason)
}

/// Clone the accessor, bind its parameters to this use, and replace `site` in place.
///
/// Returns property accesses that the clone introduced, so a nested inline accessor is expanded
/// with this use’s substitutions already applied.
fn expand_inline_accessor(
    ir: &mut crate::ir::IrFile,
    site: ExprId,
    access: &PropertyAccess,
    splice: &IrInlinePropertySplice,
    function: crate::ir::FunId,
    next_temporary: &mut u32,
) -> Result<Vec<ExprId>, FirFileLoweringFailure> {
    let accessor = splice.accessor;
    let Some(inner) = accessor_inner_body(ir, function, access.value.is_some()) else {
        return Err(failure(accessor, InlineFail::MissingBody));
    };
    let Some(operands) = access_operands(ir, function, access) else {
        return Err(failure(accessor, InlineFail::OperandMismatch));
    };
    let has_dispatch = ir
        .functions
        .get(function as usize)
        .is_some_and(|function| function.dispatch_receiver.is_some());
    let parameter_count =
        u32::try_from(operands.len()).map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    let declared = ir
        .functions
        .get(function as usize)
        .map(|function| function.params.len() + usize::from(has_dispatch))
        .unwrap_or(usize::MAX);
    if declared != operands.len() {
        return Err(failure(accessor, InlineFail::ArityMismatch));
    }
    let (bindings, reified_bindings) = recorded_bindings(&splice.substitutions);
    let operand_types = ir.functions[function as usize]
        .dispatch_receiver
        .map(Ty::obj_name)
        .into_iter()
        .chain(ir.functions[function as usize].params.iter().copied())
        .map(|ty| ty_subst_keep_unbound(ty, &bindings))
        .collect::<Vec<_>>();
    let read = crate::ir::read_values(ir, inner);
    for (ordinal, operand) in operands.iter().enumerate() {
        if !read.contains(&(ordinal as u32)) {
            ir.mark_unread_inline_operand(*operand);
        }
    }
    let identities = ir
        .function_parameter_identities(function)
        .map(<[_]>::to_vec);
    let mut operand_slots = Vec::with_capacity(operands.len());
    let mut declarations = Vec::with_capacity(operands.len());
    for (ordinal, (operand, ty)) in operands.into_iter().zip(operand_types).enumerate() {
        let slot = allocate_temporary(next_temporary)?;
        let declaration = ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: stored_value_ty(ty),
            init: Some(operand),
            named: true,
        });
        ir.call_operand_bindings.insert(declaration);
        if let Some((name, role)) = operand_debug(&identities, has_dispatch, ordinal) {
            if let Some(name) = name {
                ir.value_names.insert(declaration, name);
            }
            ir.set_debug_local_provenance(
                declaration,
                IrDebugLocalProvenance::inline_value(role, 1),
            );
        }
        declarations.push(declaration);
        operand_slots.push(Some(slot));
    }

    let protected = protected_lambda_bodies(ir, inner);
    let (cloned_root, cloned) = crate::ir::clone_expression_dag(ir, inner);
    let highest_local = cloned
        .values()
        .filter(|copy| !protected.contains(*copy))
        .filter_map(|copy| value_indices(ir.expr(*copy)).into_iter().max())
        .filter(|index| *index >= parameter_count)
        .max();
    let local_count = highest_local
        .map(|highest| highest + 1 - parameter_count)
        .unwrap_or(0);
    let local_base = *next_temporary;
    *next_temporary = local_base
        .checked_add(local_count)
        .ok_or(FirFileLoweringFailure::ValueIdentityOverflow)?;

    let mut copies = cloned
        .iter()
        .map(|(&source, &copy)| (source, copy))
        .collect::<Vec<_>>();
    copies.sort_by_key(|&(_, copy)| copy);
    let mut returns = Vec::new();
    for &(source, copy) in &copies {
        ir.mark_inline_copy(copy);
        specialize_inline_copy(ir, copy, &bindings, &reified_bindings)
            .ok_or_else(|| failure(accessor, InlineFail::MissingCopy))?;
        carry_nested_splice(ir, source, copy, &bindings);
        if protected.contains(&source) {
            // A nested lambda's template keeps its own numbering, but a return it carries that its
            // boundaries already moved to this accessor leaves this expansion too.
            if let (IrExpr::Return(value), Some(0)) = (
                ir.expr(copy).clone(),
                ir.checked_return_depths.get(&copy).copied(),
            ) {
                returns.push(InlineReturn {
                    returned: copy,
                    value,
                    nested: true,
                });
                ir.checked_return_depths.remove(&copy);
            }
            continue;
        }
        if rebase_values(
            ir.exprs.get_mut(copy as usize).unwrap(),
            parameter_count,
            &operand_slots,
            local_base,
        )
        .is_none()
        {
            return Err(failure(accessor, InlineFail::ValueRebase));
        }
        if let IrExpr::Return(value) = ir.expr(copy).clone() {
            returns.push(InlineReturn {
                returned: copy,
                value,
                nested: false,
            });
            ir.checked_return_depths.remove(&copy);
        }
    }

    let result_ty = ty_subst_keep_unbound(ir.functions[function as usize].ret, &bindings);
    let setter = access.value.is_some();
    let close_line = ir.fn_close_lines.get(&function).copied();
    let produced = finish_accessor_expansion(
        ir,
        cloned_root,
        &returns,
        result_ty,
        setter,
        close_line,
        next_temporary,
    )?;
    let mut statements = declarations;
    let value = match produced {
        ExpandedAccessor::Value(value) => value,
        ExpandedAccessor::Statement(statement) => {
            statements.push(statement);
            ir.add_expr(IrExpr::UnitInstance)
        }
    };
    ir.exprs[site as usize] = IrExpr::Block {
        stmts: statements,
        value: Some(value),
    };
    ir.inline_regions.insert(site);
    crate::trace_compiler!(
        "lower",
        "spliced inline property accessor function={function} at {site} bindings={bindings:?}"
    );
    let nested = cloned
        .values()
        .copied()
        .filter(|copy| ir.inline_property_access.splices.contains_key(copy))
        .collect();
    Ok(nested)
}

fn recorded_bindings(
    substitutions: &[IrInlineTypeSubstitution],
) -> (HashMap<String, Ty>, HashMap<String, Ty>) {
    let bindings = substitutions
        .iter()
        .map(|substitution| (substitution.name.clone(), substitution.value))
        .collect::<HashMap<_, _>>();
    let reified = substitutions
        .iter()
        .filter(|substitution| substitution.reified)
        .map(|substitution| (substitution.name.clone(), substitution.value))
        .collect::<HashMap<_, _>>();
    (bindings, reified)
}

fn carry_nested_splice(
    ir: &mut crate::ir::IrFile,
    source: ExprId,
    copy: ExprId,
    bindings: &HashMap<String, Ty>,
) {
    let Some(mut splice) = ir.inline_property_access.splices.get(&source).cloned() else {
        return;
    };
    for substitution in &mut splice.substitutions {
        substitution.value = ty_subst_keep_unbound(substitution.value, bindings);
    }
    ir.inline_property_access.splices.insert(copy, splice);
}

enum ExpandedAccessor {
    /// The accessor’s value, already the expansion’s result.
    Value(ExprId),
    /// A setter body, or a multi-return expansion whose value is read after it runs.
    Statement(ExprId),
}

fn finish_accessor_expansion(
    ir: &mut crate::ir::IrFile,
    cloned_root: ExprId,
    returns: &[InlineReturn],
    result_ty: Ty,
    setter: bool,
    close_line: Option<u32>,
    next_temporary: &mut u32,
) -> Result<ExpandedAccessor, FirFileLoweringFailure> {
    if returns.is_empty() {
        return Ok(if setter {
            ExpandedAccessor::Statement(cloned_root)
        } else {
            ExpandedAccessor::Value(cloned_root)
        });
    }
    if let [InlineReturn {
        returned: tail,
        value,
        nested: false,
    }] = *returns
    {
        if tail == cloned_root {
            let produced = value.unwrap_or_else(|| ir.add_expr(IrExpr::UnitInstance));
            return Ok(ExpandedAccessor::Value(produced));
        }
        if produce_sole_tail_return(ir, cloned_root, tail, value, close_line) {
            return Ok(if setter {
                ExpandedAccessor::Statement(cloned_root)
            } else {
                ExpandedAccessor::Value(cloned_root)
            });
        }
    }
    let result_slot = (result_ty != Ty::Unit)
        .then(|| allocate_temporary(next_temporary))
        .transpose()?;
    let label = format!("$fir_inline_prop${cloned_root}");
    for found in returns {
        ir.exprs[found.returned as usize] = super::super::inline_returns::frame_exit(
            ir,
            &label,
            result_slot,
            found.nested,
            found.value,
            found.returned,
        );
    }
    let mut statements = Vec::new();
    if let Some(slot) = result_slot {
        let initial = ir.add_expr(IrExpr::Const(IrConst::zero_for_value_type(result_ty)));
        let declaration = ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: stored_value_ty(result_ty),
            init: Some(initial),
            named: false,
        });
        ir.inline_return_frames.insert(declaration, label.clone());
        statements.push(declaration);
    }
    let fallthrough = ir.add_expr(IrExpr::Break {
        label: Some(label.clone()),
    });
    let loop_body = ir.add_expr(IrExpr::Block {
        stmts: vec![cloned_root, fallthrough],
        value: None,
    });
    let condition = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    statements.push(ir.add_expr(IrExpr::While {
        cond: condition,
        body: loop_body,
        update: None,
        post_test: false,
        label: Some(label),
    }));
    if let Some(slot) = result_slot {
        statements.push(ir.add_expr(IrExpr::GetValue(slot)));
    }
    let block = ir.add_expr(IrExpr::Block {
        stmts: statements,
        value: None,
    });
    Ok(if setter || result_slot.is_none() {
        ExpandedAccessor::Statement(block)
    } else {
        // The loop stored the result; the last statement reads it back as the block above has no
        // value. Promote that read.
        let IrExpr::Block { mut stmts, .. } = ir.expr(block).clone() else {
            return Ok(ExpandedAccessor::Statement(block));
        };
        let value = stmts.pop();
        ir.exprs[block as usize] = IrExpr::Block { stmts, value };
        ExpandedAccessor::Value(block)
    })
}

fn allocate_temporary(next_temporary: &mut u32) -> Result<u32, FirFileLoweringFailure> {
    let slot = *next_temporary;
    *next_temporary = next_temporary
        .checked_add(1)
        .ok_or(FirFileLoweringFailure::ValueIdentityOverflow)?;
    Ok(slot)
}

fn operand_debug(
    identities: &Option<Vec<crate::ir::IrParameterIdentity>>,
    has_dispatch: bool,
    ordinal: usize,
) -> Option<(Option<String>, IrInlineLocalRole)> {
    if ordinal == 0 && has_dispatch {
        return Some((None, IrInlineLocalRole::DispatchReceiver));
    }
    let identity_index = ordinal - usize::from(has_dispatch);
    let identity = identities.as_ref()?.get(identity_index)?;
    let role = match identity.role {
        crate::ir::IrParameterRole::ExtensionReceiver => IrInlineLocalRole::ExtensionReceiver,
        _ => IrInlineLocalRole::Value,
    };
    Some((identity.source_name.clone(), role))
}

/// The body `add_accessor_function` wrapped. Getters are `return <body>`; setters are `<body>`
/// followed by a valueless return. The wrapper return belongs to the accessor method, not to the
/// expression a splice produces.
fn accessor_inner_body(
    ir: &crate::ir::IrFile,
    function: crate::ir::FunId,
    setter: bool,
) -> Option<ExprId> {
    let body = ir.functions.get(function as usize)?.body?;
    let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
        return None;
    };
    if setter {
        let &[inner, returned] = stmts.as_slice() else {
            return None;
        };
        matches!(ir.expr(returned), IrExpr::Return(None)).then_some(inner)
    } else {
        let &[returned] = stmts.as_slice() else {
            return None;
        };
        match ir.expr(returned) {
            IrExpr::Return(Some(inner)) => Some(*inner),
            _ => None,
        }
    }
}

fn access_operands(
    ir: &crate::ir::IrFile,
    function_id: crate::ir::FunId,
    access: &PropertyAccess,
) -> Option<Vec<ExprId>> {
    let function = ir.functions.get(function_id as usize)?;
    let identities = ir.function_parameter_identities(function_id)?;
    let mut operands = Vec::new();
    match (function.dispatch_receiver, access.dispatch_receiver) {
        (Some(_), Some(receiver)) => operands.push(receiver),
        (None, None) => {}
        _ => return None,
    }
    let mut contexts = access.context_arguments.iter().copied();
    let mut extension = access.extension_receiver;
    let mut value = access.value;
    for identity in identities {
        let operand = match identity.role {
            crate::ir::IrParameterRole::ContextValue
            | crate::ir::IrParameterRole::AnonymousContextParameter { .. }
            | crate::ir::IrParameterRole::ContextReceiver { .. } => contexts.next()?,
            crate::ir::IrParameterRole::ExtensionReceiver => extension.take()?,
            crate::ir::IrParameterRole::Value | crate::ir::IrParameterRole::PropertySetterValue => {
                value.take()?
            }
            _ => return None,
        };
        operands.push(operand);
    }
    if contexts.next().is_some() || extension.is_some() || value.is_some() {
        return None;
    }
    Some(operands)
}

fn protected_lambda_bodies(ir: &crate::ir::IrFile, root: ExprId) -> HashSet<ExprId> {
    let mut protected = HashSet::new();
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } = ir.expr(expression)
        {
            pending.extend(captures.iter().copied());
            if let Some(body) = inline_body {
                mark_subtree(ir, *body, &mut protected);
            }
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    protected
}

/// One past every value index a lowered body may already occupy.
///
/// Each function numbers its own parameters from zero, including parameters the body never reads,
/// so the next free index has to clear both the highest index written in the arena and the highest
/// parameter slot reserved for a function or constructor.
fn fresh_value_base(ir: &crate::ir::IrFile) -> u32 {
    let mut max_index = 0u32;
    let see = |max_index: &mut u32, index: u32| *max_index = (*max_index).max(index);
    for function in &ir.functions {
        let count = function.params.len() + usize::from(function.dispatch_receiver.is_some());
        if count > 0 {
            see(&mut max_index, (count - 1) as u32);
        }
    }
    for class in &ir.classes {
        see(&mut max_index, class.ctor_param_count);
        for constructor in &class.secondary_ctors {
            let count = constructor.prefix_params.len() + constructor.params.len();
            see(&mut max_index, u32::try_from(count).unwrap_or(u32::MAX));
        }
    }
    for expression in &ir.exprs {
        for index in value_indices(expression) {
            see(&mut max_index, index);
        }
    }
    max_index.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::splice_inline_property_accessors;
    use crate::fir::{DeclarationId, PropertyId};
    use crate::fir_lower::{FirFileLoweringFailure, InlineAccessorFailure};
    use crate::ir::{
        ExprId, IrCheckedOperation, IrExpr, IrFile, IrFunction, IrInlinePropertySplice,
    };
    use crate::types::Ty;

    fn property_read(ir: &mut IrFile) -> ExprId {
        ir.add_expr(IrExpr::Checked(IrCheckedOperation::PropertyRead {
            target: PropertyId::from_raw(0),
            dispatch_receiver: None,
            extension_receiver: None,
            context_arguments: Vec::new(),
            substitutions: Vec::new(),
        }))
    }

    fn record_splice(ir: &mut IrFile, site: ExprId, accessor: DeclarationId) {
        ir.inline_property_access.splices.insert(
            site,
            IrInlinePropertySplice {
                accessor,
                substitutions: Vec::new(),
            },
        );
    }

    #[test]
    fn a_recorded_accessor_without_a_function_is_a_lowering_failure() {
        let mut ir = IrFile::default();
        let site = property_read(&mut ir);
        let accessor = DeclarationId::from_raw(1);
        record_splice(&mut ir, site, accessor);

        let error = splice_inline_property_accessors(&mut ir).unwrap_err();

        assert_eq!(
            error,
            FirFileLoweringFailure::InlineAccessor(
                accessor,
                InlineAccessorFailure::MissingFunction
            )
        );
    }

    #[test]
    fn a_recorded_splice_on_a_non_property_expression_is_a_lowering_failure() {
        let mut ir = IrFile::default();
        let site = ir.add_expr(IrExpr::UnitInstance);
        let accessor = DeclarationId::from_raw(1);
        record_splice(&mut ir, site, accessor);

        let error = splice_inline_property_accessors(&mut ir).unwrap_err();

        assert_eq!(
            error,
            FirFileLoweringFailure::InlineAccessor(accessor, InlineAccessorFailure::InvalidSite)
        );
    }

    #[test]
    fn a_recorded_accessor_without_a_body_is_a_lowering_failure() {
        let mut ir = IrFile::default();
        let site = property_read(&mut ir);
        let accessor = DeclarationId::from_raw(1);
        record_splice(&mut ir, site, accessor);
        let function = ir.add_fun(IrFunction {
            name: "getX".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.inline_property_access
            .accessor_functions
            .insert(accessor, function);

        let error = splice_inline_property_accessors(&mut ir).unwrap_err();

        assert_eq!(
            error,
            FirFileLoweringFailure::InlineAccessor(accessor, InlineAccessorFailure::MissingBody)
        );
    }
}
