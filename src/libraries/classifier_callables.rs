//! Callable candidates contributed through a classifier rather than declared as package members.
//!
//! A classifier exposes callables in two positions: its own members and associated declarations
//! (`Owner.name`), and the constructors of an `inner` classifier reached through a value of its
//! outer class (`outer.Inner(args)`). Both are normalized here into the one [`FunctionInfo`]
//! candidate structure so every call position collects them into the same overload family.

use super::{
    FnKind, FunctionInfo, GenericReturnPolicy, GenericSig, LibraryCallable, LibraryMember,
    LibraryType, ReturnInfo,
};
use crate::types::{Ty, TypeName};

/// Identity of a constructor collected into a receiver's member level: the `inner` classifier it
/// constructs and the classifier that declares the captured outer instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundInnerConstructor {
    pub inner: TypeName,
    pub outer: TypeName,
}

/// Constructor signature as a callable: the classifier's type parameters are the constructor's
/// inference variables and its application is the result. A provider that already published a
/// generic constructor signature keeps it.
pub(crate) fn constructor_generic_signature(
    internal: TypeName,
    classifier: &LibraryType,
    constructor: &LibraryMember,
) -> Option<GenericSig> {
    if let Some(signature) = &constructor.generic_sig {
        return Some(signature.clone());
    }
    if classifier.type_params().is_empty() {
        return None;
    }
    let type_arguments = classifier
        .type_params()
        .iter()
        .enumerate()
        .map(|(ordinal, formal)| {
            let bound = classifier
                .type_param_bounds()
                .get(ordinal)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(formal, bound)
        })
        .collect::<Vec<_>>();
    Some(GenericSig {
        formals: classifier.type_params().to_vec(),
        formal_bounds: classifier.type_param_bounds().to_vec(),
        receiver: None,
        params: constructor.params.clone(),
        ret: Ty::obj_args_name(internal, &type_arguments),
        return_policy: GenericReturnPolicy::Exact,
    })
}

impl FunctionInfo {
    /// Normalize one callable exposed through a classifier (`Owner.name`) into the same candidate
    /// structure used by package, module, and lexical sources. The classifier record owns the source
    /// declaration facts; this conversion copies them once into the selected-call handle so import
    /// scope and qualified syntax cannot grow separate reconstruction paths.
    pub fn classifier_member(kind: FnKind, owner: TypeName, member: LibraryMember) -> Self {
        let physical_name = member
            .physical_name
            .clone()
            .unwrap_or_else(|| member.name.clone());
        let mut callable = LibraryCallable::library(
            member.owner.unwrap_or(owner),
            physical_name,
            member.params.clone(),
            member.ret,
            member.physical_ret,
            member.descriptor.clone(),
        );
        callable.signature = member.signature.clone();
        callable.physical_params = member.physical_params.clone();
        callable.generic_sig = member.generic_sig.clone().map(Box::new);
        callable.declared_params = member
            .generic_sig
            .as_ref()
            .map(|signature| signature.parameters_with_receiver(member.context_count));
        callable.inline_modifiers = member.call_sig.inline_modifiers.clone().into_boxed_slice();
        callable.inline = member.inline;
        callable.inline_body_plan = member.inline_body_plan.clone();
        callable.suspend = member.suspend();
        callable.owner_is_interface = member.is_interface();
        callable.member_realization = member.realization;
        callable.default_realization = member.default_realization.clone();
        callable.external_default_provider = member.external_default_provider;
        callable.nonvirtual_realization = member.nonvirtual_realization.clone();
        callable.declared_ret = member.declared_ret;
        callable.overridden_results = member.overridden_results.clone();
        callable.context_count = member.context_count;
        callable.contract = member.contract.clone();
        callable.equality_bound = member.equality_bound;
        callable.plugin_expression = member.plugin_expression;
        callable.external_identity = member.external_identity;
        callable.external_property_identity = member.external_property_identity;
        callable.reflection_name = Some(member.name.clone());

        let mut candidate = FunctionInfo::plain(kind, None, callable);
        candidate.ret = ReturnInfo::new(member.ret_nullable(), member.declared_ret);
        candidate.visibility = member.visibility;
        candidate.generic_sig = member.generic_sig.clone();
        candidate.call_sig = member.call_sig.clone();
        candidate.context_count = member.context_count;
        candidate.flags.inline = member.inline;
        candidate.flags.reified = member.reified;
        candidate.flags.suspend = member.suspend();
        candidate.flags.operator = member.is_operator();
        candidate.flags.infix = member.is_infix();
        candidate.flags.is_abstract = member.is_abstract();
        candidate.flags.is_final = member.is_final();
        candidate.flags.inherited_by_delegation = member.inherited_by_delegation();
        candidate.flags.return_value_status = member.return_value_status;
        candidate.annotations = member.annotations.clone();
        candidate.default_values = member.default_values.clone();
        candidate.stable_declaration = member.stable_declaration;
        candidate.source_member = member.source_member;
        candidate.implicit_classifier_callable = member.implicit_classifier_callable;
        candidate.associated_classifier = member.associated_classifier;
        candidate.associated_access_owner = member.associated_access_owner;
        candidate
    }

    /// Normalize one constructor of the `inner` classifier `inner` as a member candidate of its
    /// outer class; `None` for a classifier that captures no outer instance. Kotlin collects such
    /// a constructor into the receiver's member level together with the same-named member
    /// functions, so `outer.Inner(args)` and a bare `Inner(args)` on an implicit receiver run one
    /// applicability and most-specific selection over both. The candidate's result is the
    /// constructed classifier; the receiver specialization is applied by the caller exactly as for
    /// a declared member.
    pub(crate) fn receiver_bound_constructor(
        inner: TypeName,
        classifier: &LibraryType,
        mut constructor: LibraryMember,
    ) -> Option<Self> {
        let outer = classifier.outer_instance?;
        constructor.owner.get_or_insert(inner);
        constructor.generic_sig = constructor_generic_signature(inner, classifier, &constructor);
        let result = constructor
            .generic_sig
            .as_ref()
            .map_or_else(|| Ty::obj_name(inner), |signature| signature.ret);
        let mut candidate = Self::classifier_member(FnKind::Member, inner, constructor);
        candidate.callable.ret = result;
        candidate.ret = ReturnInfo::new(false, Some(result));
        candidate.bound_inner_constructor = Some(BoundInnerConstructor { inner, outer });
        Some(candidate)
    }
}
