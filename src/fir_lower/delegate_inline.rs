//! Expansion of an inline dependency convention at a delegated property's use.
//!
//! A delegated property reaches its `getValue`, `setValue` and `provideDelegate` conventions
//! while the file's accessors are materialized, where no checked body frame exists to expand a
//! call in. A convention that is an inline dependency function is expanded from the template the
//! compiler supplies for it, with the operands bound to fresh locals as an inline property
//! accessor's expansion binds them. A selected template is an obligation: a convention it cannot
//! expand is a failure, never a call of the dependency function.

use crate::fir::{ExternalCallableId, InlineKind};
use crate::fx_hash::HashMap;
use crate::inline_expansion::{
    fresh_value_base, splice_inline_body, InlineBodySplice, InlineOperand, SplicedResult,
};
use crate::ir::{ExprId, IrExpr, IrFile, SameModuleInlining};

use super::dependency_templates::DependencyTemplateCache;

/// Why a convention with a template could not be expanded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DelegateInlineFailure {
    /// The convention is inline and must be expanded, but no template exists for it.
    MissingTemplate,
    /// The template's parameters do not take the operands the delegated use supplies.
    OperandMismatch,
    /// The expansion could not be built from the template's body.
    Splice,
}

/// The operands of one delegate convention call: the delegate first (the dispatch receiver of a
/// member convention, the extension receiver of an extension one), then the value arguments.
pub(super) struct DelegateConventionCall {
    pub(super) target: ExternalCallableId,
    pub(super) inline: InlineKind,
    /// Whether the convention is an extension of the delegate rather than a member of it.
    pub(super) extension: bool,
    pub(super) operands: Vec<ExprId>,
}

/// Expand `call` from its dependency template.
///
/// `Ok(None)` when the convention is not one this file expands: it is not inline, the compiler
/// implements it, or this compilation has no template source (a classfile dependency calls it as
/// declared).
pub(super) fn expand_dependency_convention(
    ir: &mut IrFile,
    templates: &mut DependencyTemplateCache,
    call: DelegateConventionCall,
) -> Result<Option<ExprId>, DelegateInlineFailure> {
    let DelegateConventionCall {
        target,
        inline,
        extension,
        operands,
    } = call;
    if !inline.can_inline()
        || ir.same_module_inlining() != SameModuleInlining::ExpandInIr
        || templates.compiler_implemented(target)
    {
        return Ok(None);
    }
    let template = match templates.template(ir, target).cloned() {
        Some(template) => template,
        None if inline.must_inline() && templates.provides_templates() => {
            return Err(DelegateInlineFailure::MissingTemplate);
        }
        None => return Ok(None),
    };
    let function = template.function;
    let declared = ir
        .functions
        .get(function as usize)
        .ok_or(DelegateInlineFailure::OperandMismatch)?;
    let has_dispatch = declared.dispatch_receiver.is_some();
    let extension_position = ir
        .extension_receiver_fns
        .contains(&function)
        .then_some(ir.declared_parameters(function).context_count);
    // The delegate is the first operand of the template: its dispatch receiver, or the extension
    // receiver of a template that declares no dispatch receiver. A context parameter or a
    // member extension puts another operand before or around it, which a delegated use does not
    // supply.
    let delegate_leads = if extension {
        !has_dispatch && extension_position == Some(0)
    } else {
        has_dispatch && extension_position.is_none()
    };
    if !delegate_leads
        || !template.class_type_parameters.is_empty()
        || declared.params.len() + usize::from(has_dispatch) != operands.len()
    {
        return Err(DelegateInlineFailure::OperandMismatch);
    }
    let body = declared
        .body
        .ok_or(DelegateInlineFailure::OperandMismatch)?;
    let bindings = HashMap::default();
    let mut next_temporary = fresh_value_base(ir);
    let spliced = splice_inline_body(
        ir,
        InlineBodySplice {
            function,
            body,
            operands: operands.into_iter().map(InlineOperand::Value).collect(),
            bindings: &bindings,
            reified_bindings: &bindings,
            statement: false,
            label_prefix: "$delegate_inline$",
        },
        &mut next_temporary,
    )
    .map_err(|_| DelegateInlineFailure::Splice)?;
    let mut statements = spliced.statements;
    let value = match spliced.produced {
        SplicedResult::Value(value) => value,
        SplicedResult::Statement(statement) => {
            statements.push(statement);
            ir.add_expr(IrExpr::UnitInstance)
        }
    };
    let expanded = ir.add_expr(IrExpr::Block {
        stmts: statements,
        value: Some(value),
    });
    ir.inline_regions.insert(expanded);
    ir.external_inline_expansions.insert(expanded);
    Ok(Some(expanded))
}
