//! Inline functions the unit's bodies call.
//!
//! kotlinc expands each call of an inline function of a library it links, which a library compiled
//! before Kotlin 2.4.20 still serializes as a call. The unit lowers such a function from its
//! serialized body like any other, keeps it out of the emitted program, and expands every call of
//! it from that lowered body, as common lowering expands a call of an inline function it compiles.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::metadata::klib_ir::tree::KlibIrFunction;

use super::super::decline::KlibBodyDeclineReason;
use super::super::function_lowering::FunctionHeader;

/// Whether the serialized `function` is inline: every call of it is expanded, never made.
fn declares_inline(function: &KlibIrFunction) -> bool {
    function.base.flags & crate::metadata::function_flags::IS_INLINE != 0
}

/// Declare `function` as an inline callee when it is inline, unless it is the selected `root`:
/// the root is lowered as the declaration it is, and a call of an inline callee is expanded.
pub(super) fn declare_callee(
    ir: &mut IrFile,
    function: FunId,
    declaration: &KlibIrFunction,
    header: &FunctionHeader,
    root: bool,
) -> Result<(), KlibBodyDeclineReason> {
    if root || !declares_inline(declaration) {
        return Ok(());
    }
    declare(ir, function, declaration, header)
}

/// Declare `function`, lowered from the serialized inline `declaration` with `header`, as one the
/// unit expands at each call. A form whose expansion is not modelled declines the call.
pub(super) fn declare(
    ir: &mut IrFile,
    function: FunId,
    declaration: &KlibIrFunction,
    header: &FunctionHeader,
) -> Result<(), KlibBodyDeclineReason> {
    if !declaration.type_parameters.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a call of a generic inline function",
        ));
    }
    if declaration.base.flags & crate::metadata::function_flags::IS_SUSPEND != 0 {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a call of a suspend inline function",
        ));
    }
    if header
        .params()
        .iter()
        .any(|parameter| parameter.non_null().is_fun())
    {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a call of an inline function with a function-typed parameter",
        ));
    }
    ir.inline_fns.insert(function);
    ir.inline_only_fns.insert(function);
    Ok(())
}

/// Expand every call of an inline function of the unit in `ir`, including the calls a copied body
/// itself makes.
pub(super) fn expand_calls(ir: &mut IrFile) -> Result<(), KlibBodyDeclineReason> {
    let mut sites = Vec::new();
    for site in 0..ir.exprs.len() as ExprId {
        if inline_callee(ir, site)?.is_some() {
            sites.push(site);
        }
    }
    if sites.is_empty() {
        return Ok(());
    }
    let mut next_temporary = crate::inline_expansion::fresh_value_base(ir);
    let mut stack = HashSet::default();
    for site in sites {
        expand(ir, site, &mut next_temporary, &mut stack)?;
    }
    Ok(())
}

/// The inline function of the unit `site` calls, when it is such a call. A call that leaves
/// arguments to an inline function's defaults is not expanded, so it declines.
fn inline_callee(ir: &IrFile, site: ExprId) -> Result<Option<FunId>, KlibBodyDeclineReason> {
    let IrExpr::Call { callee, .. } = ir.expr(site) else {
        return Ok(None);
    };
    match **callee {
        Callee::Local(function) => Ok(ir.inline_only_fns.contains(&function).then_some(function)),
        Callee::LocalWithDefaults { function, .. } if ir.inline_only_fns.contains(&function) => {
            Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of an inline function that leaves arguments to its defaults",
            ))
        }
        _ => Ok(None),
    }
}

fn expand(
    ir: &mut IrFile,
    site: ExprId,
    next_temporary: &mut u32,
    stack: &mut HashSet<FunId>,
) -> Result<(), KlibBodyDeclineReason> {
    let Some(function) = inline_callee(ir, site)? else {
        return Ok(());
    };
    let IrExpr::Call {
        dispatch_receiver,
        args,
        ..
    } = ir.expr(site).clone()
    else {
        unreachable!("the site was proved to be a call")
    };
    let Some(body) = ir
        .functions
        .get(function as usize)
        .and_then(|function| function.body)
    else {
        return Err(KlibBodyDeclineReason::NoBody);
    };
    if !stack.insert(function) {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a recursive inline function",
        ));
    }
    let operands = dispatch_receiver
        .into_iter()
        .chain(args)
        .collect::<Vec<_>>();
    let none = HashMap::default();
    let spliced = crate::inline_expansion::splice_inline_body(
        ir,
        crate::inline_expansion::InlineBodySplice {
            function,
            body,
            operands,
            bindings: &none,
            reified_bindings: &none,
            statement: false,
            label_prefix: "$klib_inline$",
        },
        next_temporary,
    )
    .map_err(|failure| {
        KlibBodyDeclineReason::SignatureMismatch(format!(
            "an inline function body cannot be expanded at its call: {failure:?}"
        ))
    })?;
    let mut statements = spliced.statements;
    let value = match spliced.produced {
        crate::inline_expansion::SplicedResult::Value(value) => value,
        crate::inline_expansion::SplicedResult::Statement(statement) => {
            statements.push(statement);
            ir.add_expr(IrExpr::UnitInstance)
        }
    };
    ir.exprs[site as usize] = IrExpr::Block {
        stmts: statements,
        value: Some(value),
    };
    ir.inline_regions.insert(site);
    let mut nested = Vec::new();
    for &copy in spliced.copies.values() {
        if inline_callee(ir, copy)?.is_some() {
            nested.push(copy);
        }
    }
    nested.sort_unstable();
    for nested in nested {
        expand(ir, nested, next_temporary, stack)?;
    }
    stack.remove(&function);
    Ok(())
}
