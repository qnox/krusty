//! A delegate convention (`getValue`, `setValue`, `provideDelegate`) selected from a dependency.
//!
//! The checker selected the declaration exactly as it selects an ordinary call of it, so it is
//! lowered as that call: through the one external-call path, which expands an inline declaration
//! from its checked plan or dependency template and records the same call facts. A delegated
//! property's accessor has no checked body of its own; it is lowered by a body-less
//! [`BodyLowering`] whose temporaries start above every value the file already uses.

use super::{BodyLowering, BodyScopes, FirFileLoweringFailure};
use crate::fir::{FirBody, FirCallTarget, FirDelegateCall, ResolvedModuleIndex};
use crate::ir::{ExprId, IrCheckedArgument, IrFile};

/// The operands of one convention call, already evaluated in source order.
pub(super) struct DelegateOperands<'a> {
    /// The value the convention's receiver slot takes: the stored delegate, or the delegate
    /// expression for `provideDelegate`.
    pub(super) delegate: ExprId,
    /// The implicit receiver that owns a member-extension convention.
    pub(super) dispatch: Option<ExprId>,
    /// The value arguments, in the convention's value-parameter order.
    pub(super) arguments: &'a [ExprId],
}

impl BodyLowering<'_> {
    /// Lower a convention whose checked target is a dependency declaration. `None` when the
    /// target is not one, or when its checked call cannot be realized; it is never retried as
    /// another call shape.
    pub(super) fn external_delegate_call(
        &mut self,
        call: &FirDelegateCall,
        operands: DelegateOperands<'_>,
        source_line: Option<u32>,
    ) -> Option<ExprId> {
        let FirCallTarget::External {
            declaration,
            singleton_dispatch,
            default_provider,
            receiver,
            declared_receiver,
            declaring_receiver,
            parameters,
            result,
            declared_result,
            overridden_results,
            semantic_role,
            intrinsic_const: _,
            overridden_declarations,
            suspend,
            inline,
            inline_plan,
            extension_receiver_parameter,
        } = &call.target
        else {
            return None;
        };
        let DelegateOperands {
            delegate,
            dispatch,
            arguments,
        } = operands;
        // A member extension's checked parameters hold its extension receiver at one slot; the
        // value arguments are numbered around it, as an ordinary checked call numbers them.
        let checked = arguments
            .iter()
            .enumerate()
            .map(|(position, &value)| {
                let mut parameter = u32::try_from(position).ok()?;
                if extension_receiver_parameter.is_some_and(|slot| parameter >= slot) {
                    parameter = parameter.checked_add(1)?;
                }
                Some(IrCheckedArgument::Expression {
                    parameter,
                    value,
                    context: Default::default(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let (dispatch_receiver, extension_receiver) = match (call.extension, dispatch) {
            (true, dispatch) => (dispatch, Some(delegate)),
            (false, None) => (Some(delegate), None),
            (false, Some(_)) => return None,
        };
        self.external_call(super::source_calls::ExternalCallRequest {
            target: *declaration,
            singleton_dispatch: *singleton_dispatch,
            default_provider: *default_provider,
            receiver_ty: *receiver,
            declared_receiver: *declared_receiver,
            declaring_receiver: *declaring_receiver,
            parameters,
            result: *result,
            declared_result: *declared_result,
            overridden_results,
            semantic_role: *semantic_role,
            overridden_declarations,
            suspend: *suspend,
            inline: *inline,
            inline_plan: inline_plan.as_deref(),
            substitutions: &call.substitutions,
            extension_receiver_parameter: *extension_receiver_parameter,
            dispatch_receiver,
            dispatch_class: None,
            extension_receiver,
            arguments: &checked,
            source_line,
        })
    }
}

/// Lower a dependency convention of a delegated property's generated accessor or storage
/// initializer, outside any checked body.
pub(super) fn generated_external_delegate_call(
    index: &ResolvedModuleIndex,
    templates: &mut super::dependency_templates::DependencyTemplateCache,
    ir: &mut IrFile,
    property: crate::fir::DeclarationId,
    call: &FirDelegateCall,
    operands: DelegateOperands<'_>,
) -> Result<ExprId, FirFileLoweringFailure> {
    let body = FirBody::new(crate::fir::BodyOwnerId::from_raw(property.raw()));
    let next_temporary = crate::inline_expansion::fresh_value_base(ir);
    let mut lowering = BodyLowering::new(
        &body,
        index,
        ir,
        false,
        BodyScopes {
            capture_slots: Default::default(),
            local_callable_scopes: vec![Default::default()],
            published_local_callables: Default::default(),
        },
        templates,
    );
    lowering.next_temporary = next_temporary;
    lowering
        .external_delegate_call(call, operands, None)
        .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(property))
}
