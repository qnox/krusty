//! Trace rendering for one checked FIR body at the common-lowering boundary.

use crate::fir::{DeclarationId, FirBody, FirExprId, FirExprKind, ResolvedModuleIndex};

pub(super) fn trace_checked_body(body: &FirBody, index: &ResolvedModuleIndex) {
    if !crate::trace::enabled("fir") {
        return;
    }
    let receiver_expressions = (0..body.expression_count())
        .filter_map(|raw| {
            let id = FirExprId::from_raw(u32::try_from(raw).ok()?);
            let expression = body.expr(id)?;
            matches!(
                expression.kind,
                FirExprKind::ImplicitReceiver { .. }
                    | FirExprKind::EnclosingReceiver { .. }
                    | FirExprKind::CapturedImplicitReceiver { .. }
                    | FirExprKind::ClassStorageRead { .. }
                    | FirExprKind::ConstructorCaptureRead { .. }
                    | FirExprKind::ConstructorContextRead { .. }
                    | FirExprKind::CapturedClassStorageRead { .. }
            )
            .then_some((id, expression.origin, expression.ty, &expression.kind))
        })
        .collect::<Vec<_>>();
    crate::trace_compiler!(
        "fir",
        "lower checked body owner={:?} declaration_name={:?} anchor={:?} local={:?} name={:?} receiver={:?} context={:?} context_values={} captures={:?} implicit_receiver_captures={:?} receiver_expressions={receiver_expressions:?}",
        body.owner(),
        index.declaration_name(DeclarationId::from_raw(body.owner().raw())),
        index.declaration_anchor(DeclarationId::from_raw(body.owner().raw())),
        body.local_callable(),
        body.debug_name(),
        body.receiver_type(),
        body.context_receiver_types(),
        body.context_value_count(),
        body.captures(),
        body.implicit_receiver_captures(),
    );
}
