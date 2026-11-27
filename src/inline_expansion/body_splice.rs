//! The expansion of one inline function body at one use: its operands bound to fresh locals, the
//! body copied in the caller's value numbering and specialized to the use's type arguments, and
//! its returns left through the frame that replaces the use.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{
    ExprId, FunId, IrConst, IrDebugLocalProvenance, IrExpr, IrFile, IrInlineLocalRole,
    IrParameterIdentity,
};
use crate::types::{stored_value_ty, Ty};

use super::result_frame::{frame_exit, produce_sole_tail_return, InlineReturn};
use super::specialization::specialize_inline_copy;
use super::value_numbering::{mark_subtree, rebase_values, value_indices};

/// One use of an inline function to expand.
pub(crate) struct InlineBodySplice<'a> {
    /// The inline function whose body is copied.
    pub(crate) function: FunId,
    /// The part of the function's body the use runs.
    pub(crate) body: ExprId,
    /// The use's operand for each of the function's parameters, its dispatch receiver first.
    pub(crate) operands: Vec<InlineOperand>,
    /// The use's type arguments, by the function's type parameter identities.
    pub(crate) bindings: &'a HashMap<String, Ty>,
    /// The reified ones among [`Self::bindings`], at their run-time types.
    pub(crate) reified_bindings: &'a HashMap<String, Ty>,
    /// Whether the use is a statement, whose expansion produces no value.
    pub(crate) statement: bool,
    /// The prefix of the label of the loop a return leaves the copied body by.
    pub(crate) label_prefix: &'a str,
}

/// What a parameter of the copied body reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InlineOperand {
    /// A value the use computes, bound to a fresh local before the copied body runs.
    Value(ExprId),
    /// A local of the caller the parameter reads in place: a value already bound to it, or a
    /// lambda whose invocations the caller expands itself.
    Slot(u32),
}

/// A use expanded: the operand declarations, then the copied body's effect or value.
pub(crate) struct SplicedInlineBody {
    pub(crate) statements: Vec<ExprId>,
    pub(crate) produced: SplicedResult,
    /// Each expression of the function's body, with its copy.
    pub(crate) copies: HashMap<ExprId, ExprId>,
}

pub(crate) enum SplicedResult {
    /// The expansion's value.
    Value(ExprId),
    /// A statement the use runs for its effect, or a frame whose result it reads after it.
    Statement(ExprId),
}

/// Why a use could not be expanded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InlineBodySpliceFailure {
    /// The use's operands do not match the function's parameters.
    ArityMismatch,
    /// A copied expression is missing from the arena.
    MissingCopy,
    /// A copied value names a parameter the use supplies no local for.
    ValueRebase,
    /// The caller's value numbering overflowed.
    ValueIdentityOverflow,
}

/// Expand `splice` with its operands bound to fresh locals from `next_temporary` on.
pub(crate) fn splice_inline_body(
    ir: &mut IrFile,
    splice: InlineBodySplice<'_>,
    next_temporary: &mut u32,
) -> Result<SplicedInlineBody, InlineBodySpliceFailure> {
    let InlineBodySplice {
        function,
        body,
        operands,
        bindings,
        reified_bindings,
        statement,
        label_prefix,
    } = splice;
    let has_dispatch = ir
        .functions
        .get(function as usize)
        .is_some_and(|function| function.dispatch_receiver.is_some());
    let parameter_count = u32::try_from(operands.len())
        .map_err(|_| InlineBodySpliceFailure::ValueIdentityOverflow)?;
    let declared = ir
        .functions
        .get(function as usize)
        .map(|function| function.params.len() + usize::from(has_dispatch))
        .unwrap_or(usize::MAX);
    if declared != operands.len() {
        return Err(InlineBodySpliceFailure::ArityMismatch);
    }
    let operand_types = ir.functions[function as usize]
        .dispatch_receiver
        .map(Ty::obj_name)
        .into_iter()
        .chain(ir.functions[function as usize].params.iter().copied())
        .map(|ty| crate::types::ty_subst_keep_unbound(ty, bindings))
        .collect::<Vec<_>>();
    let read = crate::ir::read_values(ir, body);
    for (ordinal, operand) in operands.iter().enumerate() {
        if let (InlineOperand::Value(operand), false) = (operand, read.contains(&(ordinal as u32)))
        {
            ir.mark_unread_inline_operand(*operand);
        }
    }
    let identities = ir
        .function_parameter_identities(function)
        .map(<[_]>::to_vec);
    let mut operand_slots = Vec::with_capacity(operands.len());
    let mut statements = Vec::with_capacity(operands.len());
    for (ordinal, (operand, ty)) in operands.into_iter().zip(operand_types).enumerate() {
        let operand = match operand {
            InlineOperand::Value(operand) => operand,
            InlineOperand::Slot(slot) => {
                operand_slots.push(Some(slot));
                continue;
            }
        };
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
        statements.push(declaration);
        operand_slots.push(Some(slot));
    }

    let protected = protected_lambda_bodies(ir, body);
    let (cloned_root, cloned) = crate::ir::clone_expression_dag(ir, body);
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
        .ok_or(InlineBodySpliceFailure::ValueIdentityOverflow)?;

    let mut copies = cloned
        .iter()
        .map(|(&source, &copy)| (source, copy))
        .collect::<Vec<_>>();
    copies.sort_by_key(|&(_, copy)| copy);
    let mut returns = Vec::new();
    for &(source, copy) in &copies {
        ir.mark_inline_copy(copy);
        specialize_inline_copy(ir, copy, bindings, reified_bindings)
            .ok_or(InlineBodySpliceFailure::MissingCopy)?;
        if protected.contains(&source) {
            // A nested lambda's template keeps its own numbering, but a return it carries that its
            // boundaries already moved to this function leaves this expansion too.
            if let (IrExpr::Return(value), Some(0)) = (
                ir.expr(copy).clone(),
                ir.checked_return_depths.get(&copy).copied(),
            ) {
                super::leaves_inline_frame(ir, copy);
                returns.push(InlineReturn {
                    returned: copy,
                    value,
                    nested: true,
                });
            }
            continue;
        }
        rebase_values(
            ir.exprs
                .get_mut(copy as usize)
                .ok_or(InlineBodySpliceFailure::MissingCopy)?,
            parameter_count,
            &operand_slots,
            local_base,
        )
        .ok_or(InlineBodySpliceFailure::ValueRebase)?;
        if let IrExpr::Return(value) = ir.expr(copy).clone() {
            if super::leaves_inline_frame(ir, copy) {
                returns.push(InlineReturn {
                    returned: copy,
                    value,
                    nested: false,
                });
            }
        }
    }

    let result_ty =
        crate::types::ty_subst_keep_unbound(ir.functions[function as usize].ret, bindings);
    let close_line = ir.function_position(function).close_line;
    let produced = finish_expansion(
        ir,
        FinishedCopy {
            root: cloned_root,
            returns: &returns,
            result_ty,
            statement,
            close_line,
            label: format!("{label_prefix}{cloned_root}"),
        },
        next_temporary,
    )?;
    Ok(SplicedInlineBody {
        statements,
        produced,
        copies: cloned,
    })
}

/// The copied body of an expansion and the returns it leaves through.
struct FinishedCopy<'a> {
    root: ExprId,
    returns: &'a [InlineReturn],
    result_ty: Ty,
    statement: bool,
    close_line: Option<u32>,
    label: String,
}

fn finish_expansion(
    ir: &mut IrFile,
    copy: FinishedCopy<'_>,
    next_temporary: &mut u32,
) -> Result<SplicedResult, InlineBodySpliceFailure> {
    let FinishedCopy {
        root: cloned_root,
        returns,
        result_ty,
        statement,
        close_line,
        label,
    } = copy;
    if returns.is_empty() {
        return Ok(if statement {
            SplicedResult::Statement(cloned_root)
        } else {
            SplicedResult::Value(cloned_root)
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
            return Ok(SplicedResult::Value(produced));
        }
        if produce_sole_tail_return(ir, cloned_root, tail, value, result_ty, close_line) {
            return Ok(if statement {
                SplicedResult::Statement(cloned_root)
            } else {
                SplicedResult::Value(cloned_root)
            });
        }
    }
    let result_slot = (result_ty != Ty::Unit)
        .then(|| allocate_temporary(next_temporary))
        .transpose()?;
    for found in returns {
        ir.exprs[found.returned as usize] = frame_exit(
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
    Ok(if statement || result_slot.is_none() {
        SplicedResult::Statement(block)
    } else {
        // The loop stored the result; the last statement reads it back as the block above has no
        // value. Promote that read.
        let IrExpr::Block { mut stmts, .. } = ir.expr(block).clone() else {
            return Ok(SplicedResult::Statement(block));
        };
        let value = stmts.pop();
        ir.exprs[block as usize] = IrExpr::Block { stmts, value };
        SplicedResult::Value(block)
    })
}

fn allocate_temporary(next_temporary: &mut u32) -> Result<u32, InlineBodySpliceFailure> {
    let slot = *next_temporary;
    *next_temporary = next_temporary
        .checked_add(1)
        .ok_or(InlineBodySpliceFailure::ValueIdentityOverflow)?;
    Ok(slot)
}

fn operand_debug(
    identities: &Option<Vec<IrParameterIdentity>>,
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

fn protected_lambda_bodies(ir: &IrFile, root: ExprId) -> HashSet<ExprId> {
    let mut protected = HashSet::default();
    let mut pending = vec![root];
    let mut seen = HashSet::default();
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

/// One past every value index a lowered file may already occupy.
///
/// Each function numbers its own parameters from zero, including parameters the body never reads,
/// so the next free index has to clear both the highest index written in the arena and the highest
/// parameter slot reserved for a function or constructor.
pub(crate) fn fresh_value_base(ir: &IrFile) -> u32 {
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
