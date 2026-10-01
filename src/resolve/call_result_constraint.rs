//! Expression-owned transformation of a selected callable result before contextual inference.

use std::borrow::Cow;

use crate::libraries::GenericSig;
use crate::types::Ty;

/// Inputs of one selected call, used to decide whether a bottom result may take the expected type.
pub(super) struct BottomInputs<'a> {
    pub(super) signature: &'a GenericSig,
    pub(super) bindings: &'a crate::symbol_resolver::GSigBinds,
    pub(super) inferred: Ty,
    pub(super) constraint: CallResultConstraint,
    pub(super) receiver: Option<Ty>,
    pub(super) shape: &'a super::ContextualCallShape,
    pub(super) parameters: &'a [usize],
    pub(super) arg_tys: &'a [Ty],
    pub(super) whole_arrays: &'a [bool],
    pub(super) args: &'a [crate::ast::ExprId],
}

impl<'a> super::Checker<'a> {
    /// The call's result type. A `Nothing` solution of a type-parameter return becomes the expected
    /// result only when that result is itself a legal argument of the inputs. An error-typed input
    /// does not authorize the approximation.
    pub(super) fn contextual_result(&self, site: BottomInputs<'_>) -> Ty {
        let Some(expected) = site
            .constraint
            .selected_result_approximation(site.signature.ret)
        else {
            return site.inferred;
        };
        if site.inferred != Ty::Nothing
            || !matches!(site.signature.ret.non_null(), Ty::TyParam(..))
            || expected == Ty::Error
        {
            return site.inferred;
        }
        let Some(arguments) = declared_inputs(self, &site) else {
            return site.inferred;
        };
        if bottom_approximation_preserves_inputs(
            site.signature,
            site.bindings,
            expected,
            site.receiver,
            &arguments,
            &|actual, declared| self.receiver_is_assignable(actual, declared),
        ) {
            expected
        } else {
            site.inferred
        }
    }
}

/// Whether replacing a bottom solution with the call expression's expected result still admits
/// every input that constrained that result. The trial starts from the complete selected binding
/// set so parameters involving multiple formals and declared bounds are checked in their actual
/// specialization, not against an isolated return-formal guess. An error-typed input is not an
/// admission: it must not let the expected result hide the bottom.
fn bottom_approximation_preserves_inputs(
    signature: &GenericSig,
    bindings: &crate::symbol_resolver::GSigBinds,
    expected_result: Ty,
    extension_receiver: Option<Ty>,
    arguments: &[(Ty, Ty)],
    is_assignable: &dyn Fn(Ty, Ty) -> bool,
) -> bool {
    let Some(formal) = signature.ret.non_null().ty_param_name() else {
        return false;
    };
    let instantiated = if signature.ret.is_nullable() {
        expected_result
    } else {
        expected_result.non_null()
    };
    let mut trial = bindings.clone();
    trial.insert(formal.to_string(), instantiated);
    if !crate::symbol_resolver::generic_bindings_satisfy_bounds(signature, &trial, is_assignable) {
        return false;
    }
    let names = [formal.to_string()];
    let admits = |declared: Ty, actual: Ty| {
        if actual == Ty::Error {
            return false;
        }
        if !crate::types::ty_mentions_param(declared, &names) {
            return true;
        }
        let specialized = crate::symbol_resolver::ty_subst_keep_unbound(declared, &trial);
        is_assignable(actual, specialized)
    };
    if let Some(declared) = signature.receiver {
        let Some(actual) = extension_receiver else {
            return false;
        };
        if !admits(declared, actual) {
            return false;
        }
    }
    arguments
        .iter()
        .all(|(declared, actual)| admits(*declared, *actual))
}

fn declared_inputs(checker: &super::Checker<'_>, site: &BottomInputs<'_>) -> Option<Vec<(Ty, Ty)>> {
    let context = site
        .shape
        .context_actual_types
        .iter()
        .enumerate()
        .filter_map(|(parameter, actual)| {
            Some((*site.signature.params.get(parameter)?, (*actual)?))
        })
        .collect::<Vec<_>>();
    let mut arguments = site
        .parameters
        .iter()
        .enumerate()
        .map(|(argument, &visible)| {
            let parameter = *site.shape.parameter_indices.get(visible)?;
            let mut declared = *site.signature.params.get(parameter)?;
            let actual = *site.arg_tys.get(argument)?;
            let whole_array = site.whole_arrays.get(argument).copied().unwrap_or(false)
                || checker.file.is_spread_arg(site.args[argument]);
            if site.shape.call_sig.vararg_index == Some(visible) && !whole_array {
                declared = declared.array_read_elem().unwrap_or(declared);
            }
            Some((declared, actual))
        })
        .collect::<Option<Vec<_>>>()?;
    arguments.extend(context);
    Some(arguments)
}

/// The relation between a declaration return and the type expected for the whole call expression.
///
/// Ordinary calls expose the declaration return directly. A safe call exposes its nullable lift;
/// keeping that transform explicit prevents callers from guessing by stripping nullability from
/// the expectation, which cannot distinguish `R` from an already-nullable `R?`.
#[derive(Clone, Copy, Debug)]
pub(super) enum CallResultConstraint {
    None,
    Direct(Ty),
    SafeLifted(Ty),
}

impl CallResultConstraint {
    pub(super) fn direct(expected: Option<Ty>) -> Self {
        expected.map_or(Self::None, Self::Direct)
    }

    pub(super) fn safe_lifted(expected: Option<Ty>) -> Self {
        expected.map_or(Self::None, Self::SafeLifted)
    }

    pub(super) fn expected(self) -> Option<Ty> {
        match self {
            Self::None => None,
            Self::Direct(expected) | Self::SafeLifted(expected) => Some(expected),
        }
    }

    /// Return the declaration signature as observed at the expression boundary. Only the return is
    /// transformed; parameters, formals, and bounds remain the selected declaration contract.
    pub(super) fn signature(self, signature: &GenericSig) -> Cow<'_, GenericSig> {
        match self {
            Self::SafeLifted(_) => {
                let mut lifted = signature.clone();
                lifted.ret = Ty::nullable(lifted.ret);
                Cow::Owned(lifted)
            }
            Self::None | Self::Direct(_) => Cow::Borrowed(signature),
        }
    }

    /// Apply the expression transform to a non-signature return, used by member-extension
    /// instantiation before it unifies the result with the contextual expectation.
    pub(super) fn declaration_result(self, declared: Ty) -> Ty {
        match self {
            Self::SafeLifted(_) => Ty::nullable(declared),
            Self::None | Self::Direct(_) => declared,
        }
    }

    /// The contextual approximation corresponding to the selected callable's own result. This is
    /// used only when input constraints solved that result to bottom: a safe-call boundary is
    /// removed for a non-null declaration, but an explicitly nullable declaration retains it.
    pub(super) fn selected_result_approximation(self, declared: Ty) -> Option<Ty> {
        match self {
            Self::None => None,
            Self::Direct(expected) => Some(expected),
            Self::SafeLifted(expected) if declared.admits_null() => Some(expected),
            Self::SafeLifted(expected) => Some(expected.non_null()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CallResultConstraint;
    use crate::libraries::{GenericReturnPolicy, GenericSig};
    use crate::types::Ty;

    fn signature(ret: Ty) -> GenericSig {
        GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
            params: Vec::new(),
            ret,
            receiver: None,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn safe_lift_is_idempotent_for_an_already_nullable_return() {
        let nullable = Ty::nullable(Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any"))));
        let signature = signature(nullable);
        let lifted =
            CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String))).signature(&signature);
        assert_eq!(lifted.ret, nullable);
    }

    #[test]
    fn safe_lift_wraps_a_non_null_declaration_return() {
        let signature = signature(Ty::ty_param("T", Ty::obj("kotlin/Any")));
        let lifted =
            CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String))).signature(&signature);
        assert_eq!(lifted.ret, Ty::nullable(signature.ret));
    }

    #[test]
    fn safe_result_approximation_removes_only_the_boundary_lift() {
        let constraint = CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String)));
        assert_eq!(
            constraint.selected_result_approximation(Ty::ty_param(
                "T",
                Ty::nullable(Ty::obj("kotlin/Any")),
            )),
            Some(Ty::String),
        );
    }

    #[test]
    fn safe_result_approximation_preserves_declared_nullability() {
        let constraint = CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String)));
        assert_eq!(
            constraint.selected_result_approximation(Ty::nullable(Ty::ty_param(
                "T",
                Ty::nullable(Ty::obj("kotlin/Any")),
            ))),
            Some(Ty::nullable(Ty::String)),
        );
    }

    fn result_formal() -> Ty {
        Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")))
    }

    fn bottom_signature() -> GenericSig {
        GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
            params: vec![result_formal()],
            ret: result_formal(),
            receiver: None,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    /// Assignability that would admit every pair, including an error. The guard must still refuse
    /// the error rather than treat it as a successful input.
    fn admits_everything(_actual: Ty, _declared: Ty) -> bool {
        true
    }

    #[test]
    fn an_error_input_cannot_contextualize_a_bottom_result() {
        let signature = bottom_signature();
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        bindings.insert("T".to_string(), Ty::Nothing);
        assert!(
            !super::bottom_approximation_preserves_inputs(
                &signature,
                &bindings,
                Ty::String,
                None,
                &[(result_formal(), Ty::Error)],
                &admits_everything,
            ),
            "Ty::Error must not admit the expected result"
        );
    }

    #[test]
    fn a_matching_input_keeps_the_expected_result() {
        let signature = bottom_signature();
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        bindings.insert("T".to_string(), Ty::Nothing);
        assert!(super::bottom_approximation_preserves_inputs(
            &signature,
            &bindings,
            Ty::String,
            None,
            &[(result_formal(), Ty::String)],
            &admits_everything,
        ));
    }
}
