//! Expansion of checked inline property accessors.
//!
//! The checker records the accessor declaration and its type arguments on the use. This module
//! clones that accessor's body into the use. It does not look declarations up.

use crate::fx_hash::{HashMap, HashSet};

use crate::fir::DeclarationId;
use crate::ir::{
    ExprId, IrCheckedOperation, IrExpr, IrInlinePropertySplice, IrInlineTypeSubstitution,
};
use crate::types::{ty_subst_keep_unbound, Ty};

use super::super::{FirFileLoweringFailure, InlineAccessorFailure as InlineFail};
use crate::inline_expansion::{
    fresh_value_base, splice_inline_body, InlineBodySplice, InlineBodySpliceFailure, InlineOperand,
    SplicedResult,
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
    let mut stack = HashSet::default();
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
        IrExpr::Checked(ref checked_op)
            if let IrCheckedOperation::PropertyRead {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                ..
            } = &**checked_op =>
        {
            Some(PropertyAccess {
                dispatch_receiver: *dispatch_receiver,
                extension_receiver: *extension_receiver,
                context_arguments: context_arguments.to_vec(),
                value: None,
            })
        }
        IrExpr::Checked(ref checked_op)
            if let IrCheckedOperation::PropertyWrite {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                ..
            } = &**checked_op =>
        {
            Some(PropertyAccess {
                dispatch_receiver: *dispatch_receiver,
                extension_receiver: *extension_receiver,
                context_arguments: context_arguments.to_vec(),
                value: Some(*value),
            })
        }
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
    let (bindings, reified_bindings) = recorded_bindings(&splice.substitutions);
    let spliced = splice_inline_body(
        ir,
        InlineBodySplice {
            function,
            body: inner,
            operands: operands.into_iter().map(InlineOperand::Value).collect(),
            bindings: &bindings,
            reified_bindings: &reified_bindings,
            statement: access.value.is_some(),
            label_prefix: "$fir_inline_prop$",
        },
        next_temporary,
    )
    .map_err(|reason| match reason {
        InlineBodySpliceFailure::ArityMismatch => failure(accessor, InlineFail::ArityMismatch),
        InlineBodySpliceFailure::MissingCopy => failure(accessor, InlineFail::MissingCopy),
        InlineBodySpliceFailure::ValueRebase => failure(accessor, InlineFail::ValueRebase),
        InlineBodySpliceFailure::ValueIdentityOverflow => {
            FirFileLoweringFailure::ValueIdentityOverflow
        }
    })?;
    for (&source, &copy) in &spliced.copies {
        carry_nested_splice(ir, source, copy, &bindings);
    }
    let mut statements = spliced.statements;
    let value = match spliced.produced {
        SplicedResult::Value(value) => value,
        SplicedResult::Statement(statement) => {
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
    let nested = spliced
        .copies
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
        ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::PropertyRead {
                target: PropertyId::from_raw(0),
                dispatch_receiver: None,
                extension_receiver: None,
                context_arguments: Vec::new(),
                substitutions: Vec::new(),
            },
        )))
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
