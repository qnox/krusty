//! Provider-neutral declaration/result shape of one selected call.
//!
//! Publication sidecars consume this boundary after overload selection. They do not match call
//! origins independently or reopen a declaration to recover its generic result.

use super::ResolvedCall;
use crate::fx_hash::HashMap;
use crate::libraries::{GenericSig, TypeEnhancement};
use crate::types::{Ty, TypeName};

#[derive(Clone, Copy)]
pub(super) enum DispatchBinding {
    /// The call's checked explicit receiver supplies the declaring classifier's arguments.
    Explicit(TypeName),
    /// A selected implicit receiver already carries its semantic type.
    Implicit { owner: TypeName, actual: Ty },
}

/// Declaration-owned result facts normalized from every [`ResolvedCall`] variant once.
pub(super) struct SelectedResultTarget<'a> {
    pub(super) signature: Option<&'a GenericSig>,
    pub(super) declared: Ty,
    pub(super) dispatch: Option<DispatchBinding>,
    pub(super) receiver_parameter: bool,
    /// `None` for call kinds that do not participate in Java enhancement propagation.
    pub(super) enhancement: Option<(&'a TypeEnhancement, bool)>,
}

#[derive(Clone, Copy)]
pub(super) enum StarProjectionPolicy {
    /// Flexible publication retains the readable upper-bound shape recorded for a star.
    Descend,
    /// Enhanced nullability cannot bind a callable formal through an unknown star argument.
    Stop,
}

fn declared_result(
    signature: Option<&GenericSig>,
    selected: Ty,
    declared: Option<Ty>,
    enhanced: &crate::libraries::EnhancedResult,
) -> (Ty, Option<TypeName>) {
    if let Some((owner, declared)) = enhanced.declared {
        return (declared, Some(owner));
    }
    (
        signature
            .map(|signature| signature.ret)
            .or(declared)
            .unwrap_or(selected),
        None,
    )
}

impl ResolvedCall {
    pub(super) fn selected_result_target(&self) -> SelectedResultTarget<'_> {
        let not_null = |enhancement| enhancement == crate::libraries::ResultEnhancement::NotNull;
        match self {
            ResolvedCall::Member(selected) => {
                let signature = selected.member.generic_sig.as_ref();
                let (declared, declaring_owner) = declared_result(
                    signature,
                    selected.ret,
                    selected.member.declared_ret,
                    &selected.member.enhanced_result,
                );
                SelectedResultTarget {
                    signature,
                    declared,
                    dispatch: declaring_owner
                        .or(selected.member.owner)
                        .map(DispatchBinding::Explicit),
                    receiver_parameter: false,
                    enhancement: Some((
                        &selected.member.enhanced_result.marks,
                        not_null(selected.member.call_sig.result_enhancement),
                    )),
                }
            }
            ResolvedCall::Companion(member) => {
                let signature = member.generic_sig.as_ref();
                let (declared, declaring_owner) = declared_result(
                    signature,
                    member.ret,
                    member.declared_ret,
                    &member.enhanced_result,
                );
                SelectedResultTarget {
                    signature,
                    declared,
                    dispatch: declaring_owner.map(DispatchBinding::Explicit),
                    receiver_parameter: false,
                    enhancement: Some((
                        &member.enhanced_result.marks,
                        not_null(member.call_sig.result_enhancement),
                    )),
                }
            }
            ResolvedCall::TopLevel(selected) => {
                let signature = selected.callable.generic_sig.as_deref();
                let (declared, declaring_owner) = declared_result(
                    signature,
                    selected.callable.ret,
                    selected.callable.declared_ret,
                    &selected.callable.enhanced_result,
                );
                SelectedResultTarget {
                    signature,
                    declared,
                    dispatch: declaring_owner.map(DispatchBinding::Explicit),
                    receiver_parameter: false,
                    enhancement: Some((
                        &selected.callable.enhanced_result.marks,
                        not_null(selected.call_sig.result_enhancement),
                    )),
                }
            }
            ResolvedCall::Extension(selected) => {
                let signature = selected.callable.generic_sig.as_deref();
                let (declared, declaring_owner) = declared_result(
                    signature,
                    selected.callable.ret,
                    selected.callable.declared_ret,
                    &selected.callable.enhanced_result,
                );
                SelectedResultTarget {
                    signature,
                    declared,
                    dispatch: declaring_owner.map(DispatchBinding::Explicit),
                    receiver_parameter: true,
                    enhancement: Some((&selected.callable.enhanced_result.marks, false)),
                }
            }
            ResolvedCall::LocalFunction(selected) => {
                let signature = selected.sig.generic_sig.as_ref();
                SelectedResultTarget {
                    signature,
                    declared: signature.map_or(selected.sig.ret, |signature| signature.ret),
                    dispatch: None,
                    receiver_parameter: signature
                        .is_some_and(|signature| signature.receiver.is_some()),
                    enhancement: None,
                }
            }
            ResolvedCall::MemberExtension {
                generic_sig,
                ret,
                declared_ret,
                owner,
                dispatch_receiver,
                ..
            } => {
                let signature = generic_sig.as_deref();
                let (declared, _) = declared_result(
                    signature,
                    *ret,
                    *declared_ret,
                    &crate::libraries::EnhancedResult::NONE,
                );
                SelectedResultTarget {
                    signature,
                    declared,
                    dispatch: Some(DispatchBinding::Implicit {
                        owner: *owner,
                        actual: dispatch_receiver.ty,
                    }),
                    receiver_parameter: true,
                    enhancement: None,
                }
            }
        }
    }
}

/// Collect facts for callable type parameters from one checked actual/declaration pair. The caller
/// supplies only how its payload is viewed through a resolved supertype; wrapper and parameter-tree
/// traversal is shared by flexible-interval and enhanced-nullability publication.
pub(super) fn constrain_parameters<F: Clone>(
    formals: &[String],
    declared: Ty,
    actual: Ty,
    fact: &F,
    constraints: &mut HashMap<String, Vec<F>>,
    star_projection: StarProjectionPolicy,
    view: &mut impl FnMut(Ty, &F, TypeName) -> Option<(Ty, Vec<F>)>,
) {
    match declared {
        Ty::TyParam(name, _) if formals.iter().any(|formal| formal == name) => {
            constraints
                .entry(name.to_string())
                .or_default()
                .push(fact.clone());
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::DefinitelyNotNull(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => constrain_parameters(
            formals,
            *inner,
            actual.non_null(),
            fact,
            constraints,
            star_projection,
            view,
        ),
        Ty::StarProjection(inner) => {
            if matches!(star_projection, StarProjectionPolicy::Descend) {
                constrain_parameters(
                    formals,
                    *inner,
                    actual.non_null(),
                    fact,
                    constraints,
                    star_projection,
                    view,
                );
            }
        }
        Ty::Obj(owner, declared_arguments) if !declared_arguments.is_empty() => {
            let Some((actual, facts)) = view(actual, fact, owner) else {
                return;
            };
            let Ty::Obj(actual_owner, actual_arguments) = actual.non_null() else {
                return;
            };
            if owner != actual_owner
                || declared_arguments.len() != actual_arguments.len()
                || facts.len() != actual_arguments.len()
            {
                return;
            }
            for ((declared, actual), fact) in declared_arguments
                .iter()
                .copied()
                .zip(actual_arguments.iter().copied())
                .zip(&facts)
            {
                constrain_parameters(
                    formals,
                    declared,
                    actual,
                    fact,
                    constraints,
                    star_projection,
                    view,
                );
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_generic_result_precedes_the_representation_result() {
        let nested_flexible = Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::platform_nullable(Ty::String)],
        );
        let signature = GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: Vec::new(),
            ret: nested_flexible,
            return_policy: Default::default(),
        };

        assert_eq!(
            declared_result(
                Some(&signature),
                Ty::obj("kotlin/Any"),
                Some(Ty::obj_args("kotlin/collections/List", &[Ty::String],)),
                &crate::libraries::EnhancedResult::NONE,
            ),
            (nested_flexible, None),
        );

        let owner = crate::types::type_name("sample/Carrier");
        let enhanced_declared = Ty::obj_args("sample/Carrier", &[Ty::String]);
        assert_eq!(
            declared_result(
                Some(&signature),
                Ty::obj("kotlin/Any"),
                Some(Ty::String),
                &crate::libraries::EnhancedResult {
                    marks: TypeEnhancement::NONE,
                    declared: Some((owner, enhanced_declared)),
                },
            ),
            (enhanced_declared, Some(owner)),
        );
    }

    #[test]
    fn star_projection_policy_controls_callable_formal_binding() {
        let formals = vec!["T".to_string()];
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let declared = Ty::star_projection(Ty::ty_param("T", any));
        let mut view = |_, _: &u8, _| None;

        let mut stopped = HashMap::default();
        constrain_parameters(
            &formals,
            declared,
            Ty::String,
            &7,
            &mut stopped,
            StarProjectionPolicy::Stop,
            &mut view,
        );
        assert_eq!(stopped, HashMap::default());

        let mut descended = HashMap::default();
        constrain_parameters(
            &formals,
            declared,
            Ty::String,
            &7,
            &mut descended,
            StarProjectionPolicy::Descend,
            &mut view,
        );
        assert_eq!(descended.get("T"), Some(&vec![7]));
    }
}
