//! Resolver-facing kinds of evaluated signature arguments, including the open variables of a
//! nested call's provisional result.
//!
//! The signature graph types a nested call argument before the enclosing call is selected. A
//! formal that the nested call's own inputs do not fix (`T` of `fun <T : Any> wrap(label: String)`
//! in `update(column, wrap("x"))`) stays in that provisional type. Kotlin solves it in the
//! enclosing call's constraint system, so it is renamed to a variable the enclosing selection
//! recognizes. Formals lexically visible at the call are fixed types of the enclosing declarations
//! and keep their identity, as do the variables of an active postponed (builder) call.

use super::postponed_calls::collect_type_parameters;
use super::*;

impl ProductionSignatureSemantics<'_> {
    /// The resolver-facing kind for one evaluated signature argument. An integer LITERAL keeps its
    /// folded value so overload resolution can let it adopt a narrower integer parameter type.
    pub(super) fn call_argument_kind(
        argument: &crate::fir::ResolvedSigCallArgument<'_>,
    ) -> crate::symbol_resolver::CallArgKind {
        if argument.spread {
            return crate::symbol_resolver::CallArgKind::Spread(argument.ty.get());
        }
        if argument.lambda {
            return crate::symbol_resolver::CallArgKind::LambdaLiteral(argument.ty.get());
        }
        match argument.integer_literal {
            Some(constant) => {
                crate::symbol_resolver::CallArgKind::integer_constant(argument.ty.get(), constant)
            }
            None => nested_result_template(argument.ty.get()).map_or(
                crate::symbol_resolver::CallArgKind::Typed(argument.ty.get()),
                |generic_sig| crate::symbol_resolver::CallArgKind::ExpectedTypeCallable {
                    provisional: argument.ty.get(),
                    generic_sig: std::sync::Arc::new(generic_sig),
                },
            ),
        }
    }

    pub(super) fn open_nested_result_variables(
        &self,
        scope: crate::fir::SignatureScope,
        ty: crate::fir::ResolvedTy,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let mut mentioned = crate::fx_hash::HashSet::default();
        collect_type_parameters(ty.get(), &mut mentioned);
        let visible = self.lexically_visible_type_parameters(scope)?;
        let active = self.active_postponed_type_variables(scope);
        let open = mentioned
            .into_iter()
            .filter(|formal| {
                !visible.contains(formal)
                    && !active.iter().any(|variable| variable.as_str() == *formal)
                    && !crate::types::is_nested_result_type_variable(formal)
            })
            .map(|formal| (formal, crate::types::nested_result_type_variable(formal)))
            .collect::<crate::fx_hash::HashMap<&str, &'static str>>();
        if open.is_empty() {
            return Ok(ty);
        }
        crate::fir::ResolvedTy::new(crate::types::ty_rename_params(ty.get(), &open))
            .map_err(|_| Self::failure())
    }
}

/// The result-only template of a provisional argument whose open variables were named by
/// [`ProductionSignatureSemantics::open_nested_result_variables`]: its own inputs constrain none
/// of them, so only the enclosing call's parameter can.
pub(super) fn nested_result_template(ty: Ty) -> Option<crate::libraries::GenericSig> {
    fn collect(ty: Ty, open: &mut Vec<(&'static str, Ty)>) {
        match ty.kind() {
            TyKind::TyParam(name, bound) => {
                let name = name.as_str();
                if crate::types::is_nested_result_type_variable(name)
                    && !open.iter().any(|(formal, _)| *formal == name)
                {
                    open.push((name, bound));
                }
            }
            TyKind::Nullable(inner)
            | TyKind::PlatformNullable(inner)
            | TyKind::MutabilityFlexible(inner)
            | TyKind::InProjection(inner)
            | TyKind::OutProjection(inner) => collect(inner, open),
            TyKind::Obj(_, arguments) => {
                for argument in arguments {
                    collect(*argument, open);
                }
            }
            TyKind::Fun(signature) => {
                for parameter in &signature.params {
                    collect(*parameter, open);
                }
                collect(signature.ret, open);
            }
            _ => {}
        }
    }
    let mut open = Vec::new();
    collect(ty, &mut open);
    if open.is_empty() {
        return None;
    }
    let (formals, formal_bounds) = open
        .into_iter()
        .map(|(formal, bound)| (crate::types::TyParamName::new(formal), vec![bound]))
        .unzip();
    Some(crate::libraries::GenericSig {
        formals,
        formal_bounds,
        receiver: None,
        params: Vec::new(),
        ret: ty,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    })
}
