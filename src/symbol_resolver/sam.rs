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

/// The abstract method a functional-interface conversion implements, as the declaration identity
/// its provider published: a current-module declaration or a dependency callable, or the `invoke`
/// the interface inherits from a function-type supertype (`fun interface F : () -> Unit`), which is
/// a semantic callable shape with no declaration of its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamMethodDeclaration {
    Module(crate::fir::DeclarationId),
    External(crate::fir::ExternalCallableId),
    FunctionTypeInvoke,
}

/// The specialized callable shape of a functional-interface target.
#[derive(Clone, Debug)]
pub struct SamSignature {
    pub(crate) internal: TypeName,
    pub(crate) method: String,
    /// The selected abstract method itself. `None` only for a provider member published without a
    /// declaration identity, which a consumer that needs the identity must reject.
    pub(crate) declaration: Option<SamMethodDeclaration>,
    /// Call-site-specialized logical method shape used to type the converted function.
    pub(crate) params: Vec<Ty>,
    pub(crate) ret: Ty,
    /// Declaration shape used by backend realization. Class parameters remain open here.
    pub(crate) declared_params: Vec<Ty>,
    pub(crate) declared_ret: Ty,
    pub(crate) context_count: usize,
    pub(crate) has_receiver: bool,
    pub(crate) suspend: bool,
    /// The method's primitive result replaces a non-primitive result of a declaration it
    /// overrides, at any depth (`override fun f(): Int` of `fun f(): Any`).
    pub(crate) overrides_non_primitive_result: bool,
    /// Non-primitive results of the declarations this primitive method overrides, before the call
    /// specializes them. A type parameter that the call binds to `Int` is still one of these: its
    /// erasure is `Any`, so the override returns the wrapper. A target uses these contracts when
    /// realizing its physical bridge descriptors.
    pub(crate) overridden_non_primitive_results: Vec<Ty>,
    /// The interface is a Kotlin declaration, not a Java one.
    pub(crate) kotlin_interface: bool,
    /// Provider-normalized semantic identities parallel to `declared_params`.
    pub(crate) parameter_identities: Box<[crate::fir::ResolvedParameterIdentity]>,
}

/// One inherited declaration of an abstract-method candidate: its hierarchy depth, the member, its
/// parameters and result specialized to the target application, and its declaration identity.
type Declaration = (
    u32,
    LibraryMember,
    Vec<Ty>,
    Ty,
    Option<SamMethodDeclaration>,
);
/// Every declaration of one override slot, keyed by its name and specialized parameters.
type OverrideSlot = ((String, Vec<Ty>), Vec<Declaration>);

pub(crate) fn semantic_sam_signature(
    source: &dyn SymbolSource,
    target: Ty,
) -> Option<SamSignature> {
    let target = target.non_null();
    let internal = target.obj_internal()?;
    let target_classifier = source.classifier(internal)?;
    if !target_classifier.sam_eligible {
        return None;
    }
    let kotlin_interface = target_classifier.is_kotlin;

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
            // A SAM method receives/returns values, never use-site projection syntax. Composed
            // variance can legitimately leave more than one capture shell here: selecting
            // `Consumer<in T>` through `Holder<out X>` produces `in (out X)`. Consume the complete
            // shell so the abstract method is specialized to the value type `X`, rather than
            // publishing a top-level `out X` parameter into checked FIR.
            while let Some(projected) = argument.projection_inner() {
                *argument = projected;
            }
        }
        let occurrence_bounds = classifier_type_parameter_bounds(&classifier);
        // A function-type classifier (`FunctionN`, `SuspendFunctionN`) declares only the function
        // type's own `invoke`, whichever provider published it: the same normalized method as the
        // `invoke` a functional supertype contributes below.
        let function_type = classifier.represents_function_type();
        for member in &classifier.members {
            collect_member(
                member,
                if function_type {
                    Some(SamMethodDeclaration::FunctionTypeInvoke)
                } else {
                    member_declaration(member)
                },
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
                Some(SamMethodDeclaration::FunctionTypeInvoke),
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
        let overridden_results = declarations
            .iter()
            .filter(|(depth, ..)| *depth > nearest)
            .map(|(_, member, ..)| declared_result(member))
            .collect::<Vec<_>>();
        let nearest = declarations
            .into_iter()
            .filter(|(depth, ..)| *depth == nearest)
            .collect::<Vec<_>>();
        if nearest.iter().any(|(_, member, ..)| !member.is_abstract()) {
            continue;
        }
        let (_, member, params, ret, declaration) = nearest.into_iter().next()?;
        let mut overridden_non_primitive_results = if is_primitive(declared_result(&member)) {
            overridden_results
                .into_iter()
                .filter(|&result| !is_primitive(result))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut unique_results = Vec::new();
        for result in overridden_non_primitive_results.drain(..) {
            if !unique_results.contains(&result) {
                unique_results.push(result);
            }
        }
        let overrides_non_primitive_result = !unique_results.is_empty();
        if abstract_method
            .replace((
                member,
                params,
                ret,
                declaration,
                overrides_non_primitive_result,
                unique_results,
            ))
            .is_some()
        {
            return None;
        }
    }
    let (
        sam,
        params,
        ret,
        declaration,
        overrides_non_primitive_result,
        overridden_non_primitive_results,
    ) = abstract_method?;
    let parameter_identities = sam_parameter_identities(&sam)?;
    Some(SamSignature {
        internal,
        method: sam.name.clone(),
        declaration,
        params,
        ret,
        declared_params: sam.params.clone(),
        declared_ret: sam.ret,
        context_count: sam.context_count,
        has_receiver: sam.is_member_extension(),
        suspend: sam.suspend(),
        overrides_non_primitive_result,
        overridden_non_primitive_results,
        kotlin_interface,
        parameter_identities,
    })
}

/// One identity per physical parameter of the selected abstract method.
///
/// Provider normalization inserts a typed extension receiver and publishes context slots with
/// their semantic roles. An unaligned list is an invalid declaration contract, not a reason to
/// manufacture identities after selection.
fn sam_parameter_identities(
    sam: &LibraryMember,
) -> Option<Box<[crate::fir::ResolvedParameterIdentity]>> {
    let extension_position = (sam.is_member_extension()
        && sam.params.len() == sam.call_sig.parameter_identities.len() + 1)
        .then_some(sam.context_count);
    sam.call_sig.physical_parameter_identities(
        sam.params.len(),
        sam.context_count,
        extension_position,
    )
}

/// The result a member is declared with, before any substitution.
fn declared_result(member: &LibraryMember) -> Ty {
    member
        .generic_sig
        .as_ref()
        .map_or(member.ret, |signature| signature.ret)
}

/// Kotlin's primitive types: the results a JVM override boxes when it replaces another result.
fn is_primitive(ty: Ty) -> bool {
    ty.is_numeric_or_char() || ty == Ty::Boolean
}

/// The declaration identity a provider published for a classifier member.
fn member_declaration(member: &LibraryMember) -> Option<SamMethodDeclaration> {
    match (member.stable_declaration, member.external_identity) {
        (Some(declaration), None) => Some(SamMethodDeclaration::Module(declaration)),
        (None, Some(callable)) => Some(SamMethodDeclaration::External(callable)),
        (None, None) | (Some(_), Some(_)) => None,
    }
}

fn collect_member(
    member: &LibraryMember,
    declaration: Option<SamMethodDeclaration>,
    depth: u32,
    retain_direct_kotlin_object_method: bool,
    classifier_bindings: &GSigBinds,
    classifier_occurrence_bounds: &std::collections::HashMap<String, Ty>,
    declarations: &mut Vec<OverrideSlot>,
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
    let declared_ret = declared_result(member);
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
        declarations.push((depth, member.clone(), params, ret, declaration));
    } else {
        declarations.push((
            (member.name.clone(), params.clone()),
            vec![(depth, member.clone(), params, ret, declaration)],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sam_parameter_identities_reject_an_untyped_provider_context_slot() {
        let mut member = LibraryMember::new(
            "accept".to_string(),
            vec![Ty::obj("sample/Context")],
            Ty::Unit,
            String::new(),
        );
        member.context_count = 1;
        assert_eq!(sam_parameter_identities(&member), None);

        member.call_sig.parameter_identities =
            vec![crate::fir::ResolvedParameterIdentity::ContextValue {
                ordinal: 0,
                source_name: "context".into(),
            }];
        assert!(sam_parameter_identities(&member).is_some());
    }
}
