//! Functional-interface shape discovery.
//!
//! Providers publish declarations and classifier callable shapes. This module performs the common
//! inheritance walk, specialization, and single-abstract-method selection for every declaration
//! origin.

use crate::libraries::LibraryMember;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName, Visibility};

use super::{
    classifier_bindings, classifier_type_parameter_bounds, receiver_hierarchy,
    ty_subst_keep_unbound, GSigBinds,
};

/// The specialized callable shape of a functional-interface target.
#[derive(Clone, Debug)]
pub(crate) struct SamSignature {
    pub(crate) internal: TypeName,
    pub(crate) method: String,
    pub(crate) descriptor: Option<String>,
    /// Call-site-specialized logical method shape used to type the converted function.
    pub(crate) params: Vec<Ty>,
    pub(crate) ret: Ty,
    /// Declaration shape used by backend realization. Class parameters remain open here.
    pub(crate) declared_params: Vec<Ty>,
    pub(crate) declared_ret: Ty,
    pub(crate) context_count: usize,
    pub(crate) has_receiver: bool,
    pub(crate) suspend: bool,
}

pub(crate) fn semantic_sam_signature(
    source: &dyn SymbolSource,
    target: Ty,
) -> Option<SamSignature> {
    let target = target.non_null();
    let internal = target.obj_internal()?;
    if !source.classifier(internal)?.sam_eligible {
        return None;
    }

    type Declaration = (u32, LibraryMember, Vec<Ty>, Ty);
    type OverrideSlot = ((String, Vec<Ty>), Vec<Declaration>);
    let mut declarations: Vec<OverrideSlot> = Vec::new();
    for (applied, depth) in receiver_hierarchy(source, target) {
        let Some(owner) = applied.obj_internal() else {
            continue;
        };
        let Some(classifier) = source.classifier(owner) else {
            continue;
        };
        let mut bindings = classifier_bindings(&classifier, applied);
        for argument in bindings.values_mut() {
            if let Some(projected) = argument.projection_inner() {
                *argument = projected;
            }
        }
        let occurrence_bounds = classifier_type_parameter_bounds(&classifier);
        for member in &classifier.members {
            collect_member(
                member,
                depth,
                depth == 0 && classifier.is_kotlin,
                &bindings,
                &occurrence_bounds,
                &mut declarations,
            );
        }

        // Arrow-function supertypes are semantic callable shapes, not nominal classifier edges.
        // Their inherited abstract `invoke` must nevertheless participate in the same SAM member
        // set. Materialize one short-lived normalized candidate here; no spelling or backend owner
        // is reconstructed.
        if let Some(Ty::Fun(callable)) = classifier.callable_signature.map(Ty::non_null) {
            let mut invoke = LibraryMember::new(
                "invoke".to_string(),
                callable.params.clone(),
                callable.ret,
                String::new(),
            );
            invoke.context_count = callable.context_count;
            invoke.set_is_member_extension(callable.has_receiver);
            invoke.set_suspend(callable.suspend);
            invoke.set_is_abstract(true);
            collect_member(
                &invoke,
                depth,
                false,
                &bindings,
                &occurrence_bounds,
                &mut declarations,
            );
        }
    }

    let mut abstract_method = None;
    for (_, declarations) in declarations {
        let nearest = declarations.iter().map(|(depth, ..)| *depth).min()?;
        let nearest = declarations
            .into_iter()
            .filter(|(depth, ..)| *depth == nearest)
            .collect::<Vec<_>>();
        if nearest.iter().any(|(_, member, ..)| !member.is_abstract()) {
            continue;
        }
        let (_, member, params, ret) = nearest.into_iter().next()?;
        if abstract_method.replace((member, params, ret)).is_some() {
            return None;
        }
    }
    let (sam, params, ret) = abstract_method?;
    let descriptor = (!sam.descriptor.is_empty()).then(|| sam.descriptor.clone());
    Some(SamSignature {
        internal,
        method: sam.name.clone(),
        descriptor,
        params,
        ret,
        declared_params: sam.params.clone(),
        declared_ret: sam.ret,
        context_count: sam.context_count,
        has_receiver: sam.is_member_extension(),
        suspend: sam.suspend(),
    })
}

fn collect_member(
    member: &LibraryMember,
    depth: u32,
    retain_direct_kotlin_object_method: bool,
    classifier_bindings: &GSigBinds,
    classifier_occurrence_bounds: &std::collections::HashMap<String, Ty>,
    declarations: &mut Vec<((String, Vec<Ty>), Vec<(u32, LibraryMember, Vec<Ty>, Ty)>)>,
) {
    // A public `Any`-shaped member inherited by the interface does not create a SAM method.
    // Kotlin does, however, allow the fun interface itself to redeclare that shape abstractly
    // (`fun interface F { override fun toString(): String }`).  The declaration at depth zero is
    // then the interface's functional method and must not be erased merely because its signature
    // resembles `Any`.
    if member.visibility != Visibility::Public
        || (!retain_direct_kotlin_object_method && is_public_object_method(member))
    {
        return;
    }
    let mut bindings = classifier_bindings.clone();
    let mut occurrence_bounds = classifier_occurrence_bounds.clone();
    if let Some(signature) = &member.generic_sig {
        for formal in &signature.formals {
            bindings.remove(formal);
            occurrence_bounds.remove(formal);
        }
    }
    let declared_params = member
        .generic_sig
        .as_ref()
        .map_or(member.params.as_slice(), |signature| {
            signature.params.as_slice()
        });
    let declared_ret = member
        .generic_sig
        .as_ref()
        .map_or(member.ret, |signature| signature.ret);
    let params = declared_params
        .iter()
        .map(|parameter| sam_substitute(*parameter, &occurrence_bounds, &bindings))
        .collect::<Vec<_>>();
    let ret = sam_substitute(declared_ret, &occurrence_bounds, &bindings);
    let slot = declarations.iter_mut().find(|((name, inputs), _)| {
        name == &member.name
            && inputs.len() == params.len()
            && inputs
                .iter()
                .zip(&params)
                .all(|(&left, &right)| crate::assignable::same_flexible_type(left, right))
    });
    if let Some((_, declarations)) = slot {
        declarations.push((depth, member.clone(), params, ret));
    } else {
        declarations.push((
            (member.name.clone(), params.clone()),
            vec![(depth, member.clone(), params, ret)],
        ));
    }
}

fn sam_substitute(
    declared: Ty,
    occurrence_bounds: &std::collections::HashMap<String, Ty>,
    bindings: &GSigBinds,
) -> Ty {
    let platform_inner = match declared {
        Ty::PlatformNullable(inner) => Some(*inner),
        _ => None,
    };
    let explicit_nullable = platform_inner.and_then(|inner| match inner {
        Ty::TyParam(name, _) => bindings
            .get(name)
            .copied()
            .filter(|binding| matches!(binding, Ty::Nullable(_))),
        _ => None,
    });
    if let Some(binding) = explicit_nullable {
        return binding;
    }
    ty_subst_keep_unbound(
        crate::types::ty_with_param_bounds(declared, occurrence_bounds),
        bindings,
    )
}

fn is_public_object_method(member: &LibraryMember) -> bool {
    match (member.name.as_str(), member.params.as_slice()) {
        ("hashCode" | "toString", []) => true,
        ("equals", [parameter]) => parameter.non_null().obj_internal().is_some_and(|internal| {
            internal == crate::types::type_name("kotlin/Any")
                || internal == crate::types::type_name("java/lang/Object")
        }),
        _ => false,
    }
}
