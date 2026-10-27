//! Provider-neutral declaration/result shape of one selected call.
//!
//! Publication sidecars consume this boundary after overload selection. They do not match call
//! origins independently or reopen a declaration to recover its generic result.

use super::ResolvedCall;
use crate::fx_hash::HashMap;
use crate::libraries::{GenericSig, LibraryCallable, LibraryMember, TypeEnhancement};
use crate::types::{Ty, TyKind, TypeName};

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

/// Declaration-owned result facts retained after compact-signature call selection.  The compact
/// graph evaluates ordinary Kotlin types first; declaration publication later replays only the
/// selected declaration's generic relationship over the already-evaluated receiver and arguments.
/// No spelling or overload candidate survives in this record.
#[derive(Clone, Debug)]
pub(super) struct CompactSelectedResultPublication {
    signature: Option<GenericSig>,
    declared: Ty,
    selected: Ty,
    receiver: Option<Ty>,
    enhancement: TypeEnhancement,
    arguments: Box<[Option<CompactPublicationArgument>]>,
    explicit_type_arguments: bool,
}

#[derive(Clone, Copy, Debug)]
struct CompactPublicationArgument {
    parameter: u32,
    vararg_element: bool,
}

impl CompactSelectedResultPublication {
    pub(super) fn with_selected(mut self, selected: Ty) -> Self {
        self.selected = selected;
        self
    }

    pub(super) fn callable(
        callable: &LibraryCallable,
        parameter_by_argument: Box<[Option<u32>]>,
        whole_array_arguments: Box<[bool]>,
        explicit_type_arguments: bool,
    ) -> Self {
        let signature = callable.generic_sig.as_deref();
        let (declared, _) = declared_result(
            signature,
            callable.ret,
            callable.declared_ret,
            &callable.enhanced_result,
        );
        Self {
            signature: signature.cloned(),
            declared,
            selected: callable.ret,
            receiver: signature.and_then(|signature| signature.receiver),
            enhancement: callable.enhanced_result.marks.clone(),
            arguments: publication_arguments(
                parameter_by_argument,
                callable.vararg_index,
                whole_array_arguments,
            ),
            explicit_type_arguments,
        }
    }

    pub(super) fn member(
        member: &LibraryMember,
        receiver: Option<Ty>,
        parameter_by_argument: Box<[Option<u32>]>,
        whole_array_arguments: Box<[bool]>,
        explicit_type_arguments: bool,
    ) -> Self {
        let signature = member.generic_sig.as_ref();
        let (declared, _) = declared_result(
            signature,
            member.ret,
            member.declared_ret,
            &member.enhanced_result,
        );
        Self {
            signature: signature.cloned(),
            declared,
            selected: member.ret,
            receiver,
            enhancement: member.enhanced_result.marks.with_head(
                member.call_sig.result_enhancement == crate::libraries::ResultEnhancement::NotNull,
            ),
            arguments: publication_arguments(
                parameter_by_argument,
                member.call_sig.vararg_index,
                whole_array_arguments,
            ),
            explicit_type_arguments,
        }
    }

    pub(super) fn constructor(
        owner: TypeName,
        formals: Vec<String>,
        bounds: Vec<Ty>,
        parameters: Vec<Ty>,
        selected: Ty,
        parameter_by_argument: Box<[Option<u32>]>,
        vararg: Option<usize>,
        whole_array_arguments: Box<[bool]>,
        explicit_type_arguments: bool,
    ) -> Self {
        let declared_arguments = formals
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                Ty::ty_param(
                    formal,
                    bounds
                        .get(index)
                        .copied()
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                )
            })
            .collect::<Vec<_>>();
        Self {
            signature: Some(GenericSig {
                formals,
                formal_bounds: bounds.into_iter().map(|bound| vec![bound]).collect(),
                receiver: None,
                params: parameters,
                ret: Ty::obj_args_name(owner, &declared_arguments),
                return_policy: Default::default(),
            }),
            declared: Ty::obj_args_name(owner, &declared_arguments),
            selected,
            receiver: None,
            enhancement: TypeEnhancement::NONE,
            arguments: publication_arguments(parameter_by_argument, vararg, whole_array_arguments),
            explicit_type_arguments,
        }
    }

    /// Project publication-only platform occurrences through this exact selected declaration.
    /// `view` is the resolver-owned hierarchy operation used when a receiver/argument reaches a
    /// declared classifier through a supertype; this module never looks a classifier up itself.
    pub(super) fn publish(
        &self,
        receiver: Option<(Ty, Ty)>,
        arguments: &[(Ty, Ty)],
        mut view: impl FnMut(Ty, Ty, TypeName) -> Option<(Ty, Vec<Ty>)>,
    ) -> Ty {
        let callable_formals = self
            .signature
            .as_ref()
            .map(|signature| signature.formals.clone())
            .unwrap_or_default();
        let mut receiver_formals = Vec::new();
        if let Some(receiver) = self.receiver {
            collect_type_parameters(receiver, &mut receiver_formals);
        }
        receiver_formals.retain(|formal| !callable_formals.contains(formal));
        let mut formals = callable_formals;
        for formal in &receiver_formals {
            if !formals.contains(formal) {
                formals.push(formal.clone());
            }
        }

        let mut constraints: HashMap<String, Vec<Ty>> = HashMap::default();
        let mut publication_view =
            |actual, publication: &Ty, owner| view(actual, *publication, owner);
        if let (Some(declared), Some((actual, publication))) = (self.receiver, receiver) {
            let receiver_constraints = if self.explicit_type_arguments {
                receiver_formals.as_slice()
            } else {
                formals.as_slice()
            };
            constrain_parameters(
                receiver_constraints,
                declared,
                actual,
                &publication,
                &mut constraints,
                StarProjectionPolicy::Descend,
                &mut publication_view,
            );
        }
        if !self.explicit_type_arguments {
            if let Some(signature) = &self.signature {
                for (argument, binding) in arguments.iter().zip(&self.arguments) {
                    let Some(binding) = binding else {
                        continue;
                    };
                    let Some(parameter) = signature.params.get(binding.parameter as usize).copied()
                    else {
                        continue;
                    };
                    let parameter = if binding.vararg_element {
                        parameter.array_read_elem().unwrap_or(parameter)
                    } else {
                        parameter
                    };
                    constrain_parameters(
                        &formals,
                        parameter,
                        argument.0,
                        &argument.1,
                        &mut constraints,
                        StarProjectionPolicy::Descend,
                        &mut publication_view,
                    );
                }
            }
        }
        let bindings = constraints
            .into_iter()
            .filter_map(|(formal, values)| {
                let first = *values.first()?;
                let publication = values.iter().copied().skip(1).fold(first, meet_publication);
                Some((formal, publication))
            })
            .collect::<HashMap<_, _>>();
        let published = publish_selected(self.declared, self.selected, &bindings);
        suppress_enhanced_flexibility(self.selected, published, &self.enhancement)
    }
}

/// Java enhancement has already selected a rigid semantic face at these exact positions. Compact
/// declaration publication may restore unenhanced flexible intervals around them, but must not
/// turn an enhanced position back into a platform type.
fn suppress_enhanced_flexibility(selected: Ty, published: Ty, enhancement: &TypeEnhancement) -> Ty {
    match (selected.kind(), published.kind()) {
        // Mutability flexibility is not a nesting level: the enhancement applies to its classifier.
        (TyKind::MutabilityFlexible(selected), TyKind::MutabilityFlexible(published)) => {
            Ty::mutability_flexible(suppress_enhanced_flexibility(
                selected,
                published,
                enhancement,
            ))
        }
        (TyKind::Obj(selected_owner, selected), TyKind::Obj(owner, published))
            if selected_owner == owner && selected.len() == published.len() =>
        {
            Ty::obj_args_name(
                owner,
                &selected
                    .iter()
                    .copied()
                    .zip(published.iter().copied())
                    .enumerate()
                    .map(|(index, (selected, published))| {
                        suppress_enhanced_flexibility(
                            selected,
                            published,
                            enhancement.argument(index),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        }
        (TyKind::Fun(selected), TyKind::Fun(published))
            if selected.context_count == published.context_count
                && selected.has_receiver == published.has_receiver
                && selected.suspend == published.suspend
                && selected.params.len() == published.params.len() =>
        {
            Ty::fun_with_shape(
                selected
                    .params
                    .iter()
                    .copied()
                    .zip(published.params.iter().copied())
                    .enumerate()
                    .map(|(index, (selected, published))| {
                        suppress_enhanced_flexibility(
                            selected,
                            published,
                            enhancement.argument(index),
                        )
                    })
                    .collect(),
                suppress_enhanced_flexibility(
                    selected.ret,
                    published.ret,
                    enhancement.argument(selected.params.len()),
                ),
                published.context_count,
                published.has_receiver,
                published.suspend,
            )
        }
        (TyKind::Nullable(selected), TyKind::Nullable(published)) => Ty::nullable(
            suppress_enhanced_flexibility(selected, published, enhancement)
                .non_null_keeping_mutability(),
        ),
        (_, TyKind::PlatformNullable(published)) if !selected.is_nullable() => {
            let published = suppress_enhanced_flexibility(selected, published, enhancement)
                .platform_lower_bound();
            if enhancement.head() {
                published
            } else {
                Ty::platform_nullable(published)
            }
        }
        (_, _) if enhancement.head() => selected,
        (_, _) => published,
    }
}

fn publication_arguments(
    parameter_by_argument: Box<[Option<u32>]>,
    vararg: Option<usize>,
    whole_array_arguments: Box<[bool]>,
) -> Box<[Option<CompactPublicationArgument>]> {
    parameter_by_argument
        .iter()
        .copied()
        .enumerate()
        .map(|(argument, parameter)| {
            parameter.map(|parameter| CompactPublicationArgument {
                parameter,
                vararg_element: vararg == Some(parameter as usize)
                    && !whole_array_arguments
                        .get(argument)
                        .copied()
                        .unwrap_or(false),
            })
        })
        .collect()
}

fn collect_type_parameters(ty: Ty, output: &mut Vec<String>) {
    match ty.kind() {
        TyKind::TyParam(name, _) => {
            if !output.iter().any(|candidate| candidate == name) {
                output.push(name.to_owned().to_string());
            }
        }
        TyKind::Obj(_, arguments) => {
            for argument in arguments {
                collect_type_parameters(*argument, output);
            }
        }
        TyKind::Fun(signature) => {
            for parameter in &signature.params {
                collect_type_parameters(*parameter, output);
            }
            collect_type_parameters(signature.ret, output);
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner)
        | TyKind::DefinitelyNotNull(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::StarProjection(inner) => collect_type_parameters(inner, output),
        _ => {}
    }
}

fn publish_selected(declared: Ty, selected: Ty, bindings: &HashMap<String, Ty>) -> Ty {
    match (declared.kind(), selected.kind()) {
        (TyKind::TyParam(name, declared_bound), _) => {
            let publication = bindings
                .get(name.as_str())
                .copied()
                .map(|publication| align_publication(publication, selected))
                .unwrap_or(selected);
            if declared_bound.is_platform_nullable() && !selected.is_nullable() {
                Ty::platform_nullable(publication.platform_lower_bound())
            } else {
                publication
            }
        }
        (TyKind::PlatformNullable(declared), TyKind::Nullable(selected)) => Ty::nullable(
            publish_selected(declared, selected, bindings).non_null_keeping_mutability(),
        ),
        (TyKind::MutabilityFlexible(declared), TyKind::MutabilityFlexible(selected)) => {
            Ty::mutability_flexible(publish_selected(declared, selected, bindings))
        }
        (TyKind::PlatformNullable(declared), TyKind::PlatformNullable(selected)) => {
            Ty::platform_nullable(
                publish_selected(declared, selected, bindings).platform_lower_bound(),
            )
        }
        (TyKind::PlatformNullable(declared), _) if !selected.is_nullable() => {
            Ty::platform_nullable(
                publish_selected(declared, selected, bindings).platform_lower_bound(),
            )
        }
        (TyKind::Nullable(declared), TyKind::Nullable(selected)) => Ty::nullable(
            publish_selected(declared, selected, bindings).non_null_keeping_mutability(),
        ),
        (TyKind::DefinitelyNotNull(declared), TyKind::DefinitelyNotNull(selected)) => Ty::from(
            TyKind::DefinitelyNotNull(publish_selected(declared, selected, bindings)),
        ),
        (TyKind::InProjection(declared), TyKind::InProjection(selected)) => {
            Ty::in_projection(publish_selected(declared, selected, bindings))
        }
        (TyKind::OutProjection(declared), TyKind::OutProjection(selected)) => {
            Ty::out_projection(publish_selected(declared, selected, bindings))
        }
        (TyKind::StarProjection(declared), TyKind::StarProjection(selected)) => {
            Ty::star_projection(publish_selected(declared, selected, bindings))
        }
        (TyKind::Obj(owner, declared), TyKind::Obj(selected_owner, selected))
            if owner == selected_owner && declared.len() == selected.len() =>
        {
            Ty::obj_args_name(
                owner,
                &declared
                    .iter()
                    .copied()
                    .zip(selected.iter().copied())
                    .map(|(declared, selected)| publish_selected(declared, selected, bindings))
                    .collect::<Vec<_>>(),
            )
        }
        (TyKind::Fun(declared), TyKind::Fun(selected))
            if declared.context_count == selected.context_count
                && declared.has_receiver == selected.has_receiver
                && declared.suspend == selected.suspend
                && declared.params.len() == selected.params.len() =>
        {
            Ty::fun_with_shape(
                declared
                    .params
                    .iter()
                    .copied()
                    .zip(selected.params.iter().copied())
                    .map(|(declared, selected)| publish_selected(declared, selected, bindings))
                    .collect(),
                publish_selected(declared.ret, selected.ret, bindings),
                selected.context_count,
                selected.has_receiver,
                selected.suspend,
            )
        }
        _ => selected,
    }
}

fn align_publication(publication: Ty, selected: Ty) -> Ty {
    match (publication.kind(), selected.kind()) {
        (TyKind::PlatformNullable(publication), TyKind::Nullable(selected)) => {
            Ty::nullable(align_publication(publication, selected).non_null_keeping_mutability())
        }
        (TyKind::PlatformNullable(publication), TyKind::PlatformNullable(selected)) => {
            Ty::platform_nullable(align_publication(publication, selected).platform_lower_bound())
        }
        (TyKind::PlatformNullable(publication), _) if !selected.is_nullable() => {
            Ty::platform_nullable(align_publication(publication, selected).platform_lower_bound())
        }
        (TyKind::Obj(owner, publication), TyKind::Obj(selected_owner, selected))
            if owner == selected_owner && publication.len() == selected.len() =>
        {
            Ty::obj_args_name(
                owner,
                &publication
                    .iter()
                    .copied()
                    .zip(selected.iter().copied())
                    .map(|(publication, selected)| align_publication(publication, selected))
                    .collect::<Vec<_>>(),
            )
        }
        (TyKind::Fun(publication), TyKind::Fun(selected))
            if publication.context_count == selected.context_count
                && publication.has_receiver == selected.has_receiver
                && publication.suspend == selected.suspend
                && publication.params.len() == selected.params.len() =>
        {
            Ty::fun_with_shape(
                publication
                    .params
                    .iter()
                    .copied()
                    .zip(selected.params.iter().copied())
                    .map(|(publication, selected)| align_publication(publication, selected))
                    .collect(),
                align_publication(publication.ret, selected.ret),
                selected.context_count,
                selected.has_receiver,
                selected.suspend,
            )
        }
        (TyKind::Nullable(publication), TyKind::Nullable(selected)) => {
            Ty::nullable(align_publication(publication, selected).non_null_keeping_mutability())
        }
        (TyKind::MutabilityFlexible(publication), TyKind::MutabilityFlexible(selected)) => {
            Ty::mutability_flexible(align_publication(publication, selected))
        }
        _ => selected,
    }
}

fn meet_publication(left: Ty, right: Ty) -> Ty {
    if left == right {
        return left;
    }
    let (left_flexible, left) = match left.kind() {
        TyKind::PlatformNullable(inner) => (true, inner),
        _ => (false, left),
    };
    let (right_flexible, right) = match right.kind() {
        TyKind::PlatformNullable(inner) => (true, inner),
        _ => (false, right),
    };
    let lower = match (left.kind(), right.kind()) {
        (TyKind::Obj(owner, left), TyKind::Obj(right_owner, right))
            if owner == right_owner && left.len() == right.len() =>
        {
            Ty::obj_args_name(
                owner,
                &left
                    .iter()
                    .copied()
                    .zip(right.iter().copied())
                    .map(|(left, right)| meet_publication(left, right))
                    .collect::<Vec<_>>(),
            )
        }
        (TyKind::Fun(left), TyKind::Fun(right))
            if left.context_count == right.context_count
                && left.has_receiver == right.has_receiver
                && left.suspend == right.suspend
                && left.params.len() == right.params.len() =>
        {
            Ty::fun_with_shape(
                left.params
                    .iter()
                    .copied()
                    .zip(right.params.iter().copied())
                    .map(|(left, right)| meet_publication(left, right))
                    .collect(),
                meet_publication(left.ret, right.ret),
                left.context_count,
                left.has_receiver,
                left.suspend,
            )
        }
        (_, _) if left == right => left,
        (_, _) => left.platform_lower_bound(),
    };
    if left_flexible && right_flexible {
        Ty::platform_nullable(lower.platform_lower_bound())
    } else {
        lower
    }
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
    match declared.kind() {
        TyKind::TyParam(name, _) if formals.iter().any(|formal| formal == name) => {
            constraints
                .entry(name.to_string())
                .or_default()
                .push(fact.clone());
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner)
        | TyKind::DefinitelyNotNull(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner) => constrain_parameters(
            formals,
            inner,
            actual.non_null(),
            fact,
            constraints,
            star_projection,
            view,
        ),
        TyKind::StarProjection(inner) => {
            if matches!(star_projection, StarProjectionPolicy::Descend) {
                constrain_parameters(
                    formals,
                    inner,
                    actual.non_null(),
                    fact,
                    constraints,
                    star_projection,
                    view,
                );
            }
        }
        TyKind::Obj(owner, declared_arguments) if !declared_arguments.is_empty() => {
            let Some((actual, facts)) = view(actual, fact, owner) else {
                return;
            };
            let TyKind::Obj(actual_owner, actual_arguments) = actual.non_null().kind() else {
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

    fn direct_view(actual: Ty, publication: Ty, owner: TypeName) -> Option<(Ty, Vec<Ty>)> {
        let TyKind::Obj(actual_owner, _) = actual.non_null().kind() else {
            return None;
        };
        let TyKind::Obj(publication_owner, arguments) = publication.non_null().kind() else {
            return None;
        };
        (actual_owner == owner && publication_owner == owner)
            .then_some((actual.non_null(), arguments.to_vec()))
    }

    #[test]
    fn compact_selected_java_member_publishes_a_function_type_interval() {
        let function = Ty::fun(vec![Ty::String], Ty::String);
        let variable = Ty::ty_param("E", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        let list = crate::types::type_name("kotlin/collections/List");
        let target = CompactSelectedResultPublication {
            signature: Some(GenericSig {
                formals: Vec::new(),
                formal_bounds: Vec::new(),
                receiver: None,
                params: Vec::new(),
                ret: variable,
                return_policy: Default::default(),
            }),
            declared: variable,
            selected: function,
            receiver: Some(Ty::obj_args_name(list, &[variable])),
            enhancement: TypeEnhancement::NONE,
            arguments: Box::new([]),
            explicit_type_arguments: false,
        };
        let ordinary_receiver = Ty::obj_args_name(list, &[function]);
        let published_receiver =
            Ty::platform_nullable(Ty::obj_args_name(list, &[Ty::platform_nullable(function)]));

        assert_eq!(
            target.publish(
                Some((ordinary_receiver, published_receiver)),
                &[],
                direct_view,
            ),
            Ty::platform_nullable(function),
        );
    }

    #[test]
    fn compact_constructor_and_source_member_forward_nested_publication() {
        let rigid_list = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let published_list = Ty::platform_nullable(Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::platform_nullable(Ty::String)],
        ));
        let carrier = crate::types::type_name("sample/Carrier");
        let variable = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let constructed = Ty::obj_args_name(carrier, &[rigid_list]);
        let constructor = CompactSelectedResultPublication::constructor(
            carrier,
            vec!["T".to_owned()],
            vec![Ty::nullable(Ty::obj("kotlin/Any"))],
            vec![variable],
            constructed,
            vec![Some(0)].into_boxed_slice(),
            None,
            vec![false].into_boxed_slice(),
            false,
        );
        let published_carrier =
            constructor.publish(None, &[(rigid_list, published_list)], direct_view);
        assert_eq!(
            published_carrier,
            Ty::obj_args_name(carrier, &[published_list]),
        );

        let member = CompactSelectedResultPublication {
            signature: Some(GenericSig {
                formals: Vec::new(),
                formal_bounds: Vec::new(),
                receiver: None,
                params: Vec::new(),
                ret: variable,
                return_policy: Default::default(),
            }),
            declared: variable,
            selected: rigid_list,
            receiver: Some(Ty::obj_args_name(carrier, &[variable])),
            enhancement: TypeEnhancement::NONE,
            arguments: Box::new([]),
            explicit_type_arguments: false,
        };
        assert_eq!(
            member.publish(Some((constructed, published_carrier)), &[], direct_view,),
            published_list,
        );
    }

    #[test]
    fn explicit_callable_arguments_keep_receiver_classifier_publication() {
        let carrier = crate::types::type_name("sample/Carrier");
        let class_variable = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let method_variable = Ty::ty_param("M", Ty::nullable(Ty::obj("kotlin/Any")));
        let target = CompactSelectedResultPublication {
            signature: Some(GenericSig {
                formals: vec!["M".to_owned()],
                formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
                receiver: None,
                params: vec![method_variable],
                ret: class_variable,
                return_policy: Default::default(),
            }),
            declared: class_variable,
            selected: Ty::String,
            receiver: Some(Ty::obj_args_name(carrier, &[class_variable])),
            enhancement: TypeEnhancement::NONE,
            arguments: vec![Some(CompactPublicationArgument {
                parameter: 0,
                vararg_element: false,
            })]
            .into_boxed_slice(),
            explicit_type_arguments: true,
        };
        let ordinary_receiver = Ty::obj_args_name(carrier, &[Ty::String]);
        let published_receiver = Ty::obj_args_name(carrier, &[Ty::platform_nullable(Ty::String)]);

        assert_eq!(
            target.publish(
                Some((ordinary_receiver, published_receiver)),
                &[(Ty::String, Ty::platform_nullable(Ty::String))],
                direct_view,
            ),
            Ty::platform_nullable(Ty::String),
        );
    }

    #[test]
    fn final_evaluator_result_replaces_the_candidate_result() {
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        let target = CompactSelectedResultPublication {
            signature: Some(GenericSig {
                formals: vec!["T".to_owned()],
                formal_bounds: vec![vec![Ty::platform_nullable(Ty::obj("kotlin/Any"))]],
                receiver: None,
                params: Vec::new(),
                ret: variable,
                return_policy: Default::default(),
            }),
            declared: variable,
            selected: Ty::String,
            receiver: None,
            enhancement: TypeEnhancement::NONE,
            arguments: Box::new([]),
            explicit_type_arguments: false,
        };

        assert_eq!(
            target
                .with_selected(Ty::fun(vec![Ty::String], Ty::String))
                .publish(None, &[], direct_view),
            Ty::platform_nullable(Ty::fun(vec![Ty::String], Ty::String)),
        );
    }

    #[test]
    fn compact_publication_does_not_restore_an_enhanced_nested_interval() {
        let rigid = Ty::obj_args("kotlin/collections/Set", &[Ty::String]);
        let published = Ty::obj_args(
            "kotlin/collections/Set",
            &[Ty::platform_nullable(Ty::String)],
        );
        let enhancement = TypeEnhancement::new(false, vec![TypeEnhancement::new(true, Vec::new())]);

        assert_eq!(
            suppress_enhanced_flexibility(rigid, published, &enhancement),
            rigid,
        );
    }

    #[test]
    fn root_enhancement_preserves_unenhanced_nested_intervals() {
        let selected = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let published = Ty::platform_nullable(Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::platform_nullable(Ty::String)],
        ));
        let enhancement = TypeEnhancement::new(true, vec![TypeEnhancement::NONE]);

        assert_eq!(
            suppress_enhanced_flexibility(selected, published, &enhancement),
            Ty::obj_args(
                "kotlin/collections/List",
                &[Ty::platform_nullable(Ty::String)],
            ),
        );
    }

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
