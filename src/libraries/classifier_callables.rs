//! Callable candidates contributed through a classifier rather than declared as package members.
//!
//! A classifier exposes callables in two positions: its own members and associated declarations
//! (`Owner.name`), and the constructors of an `inner` classifier reached through a value of its
//! outer class (`outer.Inner(args)`). Both are normalized here into the one [`FunctionInfo`]
//! candidate structure so every call position collects them into the same overload family.

use super::{
    AliasExpansion, FnKind, FunctionInfo, GenericReturnPolicy, GenericSig, LibraryCallable,
    LibraryMember, LibraryType, ReturnInfo,
};
use crate::types::{Ty, TypeName};

/// Identity of a constructor collected into a receiver's member level: the `inner` classifier it
/// constructs and the classifier that declares the captured outer instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundInnerConstructor {
    pub inner: TypeName,
    pub outer: TypeName,
}

/// The alias declaration and uninstantiated callable shape retained by a selected constructor.
/// Receiver-specialized inference bounds are call constraints, not bounds declared by the alias.
#[derive(Clone)]
pub(crate) struct ConstructorAlias {
    pub(crate) declaration: AliasExpansion,
    pub(crate) signature: GenericSig,
    pub(crate) underlying_signature: GenericSig,
    pub(crate) rigid_formals: Vec<String>,
}

impl LibraryMember {
    /// The selected constructor's parameters before applicability specialization. Constructors have
    /// no extension receiver, so this source-order declaration vector is parallel to `params`;
    /// enclosing-instance and default-marker JVM slots are independently owned physical inputs.
    pub fn constructor_declaration_params(&self) -> &[Ty] {
        self.declared_params.as_deref().unwrap_or_else(|| {
            self.generic_sig
                .as_ref()
                .map(|signature| signature.params.as_slice())
                .filter(|params| params.len() == self.params.len())
                .unwrap_or(&self.params)
        })
    }
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
    Some(GenericSig {
        formals: classifier.type_params().to_vec(),
        formal_bounds: classifier.type_param_bounds().to_vec(),
        receiver: None,
        params: constructor.params.clone(),
        ret: classifier_instance_type(internal, classifier),
        return_policy: GenericReturnPolicy::Exact,
    })
}

/// A constructor's complete declared shape, including its logical classifier result before
/// reference adaptation. The physical nongeneric constructor return may instead be Unit.
pub(crate) fn constructor_declaration_signature(
    internal: TypeName,
    classifier: &LibraryType,
    constructor: &LibraryMember,
) -> GenericSig {
    constructor_generic_signature(internal, classifier, constructor).unwrap_or_else(|| GenericSig {
        formals: Vec::new(),
        formal_bounds: Vec::new(),
        receiver: None,
        params: constructor.constructor_declaration_params().to_vec(),
        ret: classifier_instance_type(internal, classifier),
        return_policy: GenericReturnPolicy::Exact,
    })
}

fn classifier_instance_type(internal: TypeName, classifier: &LibraryType) -> Ty {
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
    Ty::obj_args_name(internal, &type_arguments)
}

impl FunctionInfo {
    pub(crate) fn constructor_alias(&self) -> Option<&AliasExpansion> {
        self.constructor_alias
            .as_deref()
            .map(|alias| &alias.declaration)
    }

    /// Normalize one callable exposed through a classifier (`Owner.name`) into the same candidate
    /// structure used by package, module, and lexical sources. The classifier record owns the source
    /// declaration facts; this conversion copies them once into the selected-call handle so import
    /// scope and qualified syntax cannot grow separate reconstruction paths.
    pub fn classifier_member(kind: FnKind, owner: TypeName, member: LibraryMember) -> Self {
        let mut callable = LibraryCallable::library(
            member.owner.unwrap_or(owner),
            member.name.clone(),
            member.params.clone(),
            member.ret,
            member.physical_ret,
            member.descriptor.clone(),
        );
        callable.declaration_owner = Some(crate::types::SemanticCallableOwner::Classifier(owner));
        callable.physical_name = member.physical_name.clone();
        callable.signature = member.signature.clone();
        callable.physical_params = member.physical_params.clone();
        callable.physical_parameter_plan = member.physical_parameter_plan.clone();
        callable.generic_sig = member.generic_sig.clone().map(Box::new);
        callable.declared_params = member.declared_params.clone().or_else(|| {
            member
                .generic_sig
                .as_ref()
                .map(|signature| signature.parameters_with_receiver(member.context_count))
        });
        callable.inline_modifiers = member.call_sig.inline_modifiers.clone().into_boxed_slice();
        callable.reified_type_parameter_ordinals = member
            .call_sig
            .reified_type_parameter_ordinals
            .clone()
            .into_boxed_slice();
        callable.inline = member.inline;
        callable.inline_body_plan = member.inline_body_plan.clone();
        callable.suspend = member.suspend();
        callable.owner_is_interface = member.is_interface();
        callable.visibility = member.visibility;
        callable.member_realization = member.realization;
        callable.compiler_intrinsic = member.realization.compiler_intrinsic();
        callable.semantic_role = member.semantic_role;
        callable.overridden_declarations = member.overridden_declarations.clone();
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
        candidate.receiver = member.semantic_extension_receiver();
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
        candidate.flags.deprecated_hidden = member.deprecated_hidden();
        candidate.flags.inherited_by_delegation = member.inherited_by_delegation();
        candidate.flags.return_value_status = member.return_value_status;
        candidate.classifier_static = member.is_classifier_static();
        candidate.callable.annotations = member.annotations.clone();
        candidate.callable.deprecation = member.deprecation.clone();
        candidate.default_values = member.default_values.clone();
        candidate.stable_declaration = member.stable_declaration;
        candidate.source_member = member.source_member;
        candidate.implicit_classifier_callable = member.implicit_classifier_callable;
        candidate.associated_classifier = member.associated_classifier;
        candidate.associated_access_owner = member.associated_access_owner;
        candidate
    }

    /// Project an already-selected constructor out of its lookup facet. The synthetic enclosing
    /// receiver is a reference binding, not one of the constructor's declared parameters.
    pub(crate) fn selected_constructor_member(&self) -> Option<LibraryMember> {
        self.bound_inner_constructor?;
        let mut member = self.member_with_return(self.callable.ret);
        let mut declared = self.callable.declared_params.as_deref()?.to_vec();
        if let Some(receiver) =
            super::extension_receiver_index(self.is_extension(), self.context_count)
        {
            if receiver >= declared.len() {
                return None;
            }
            declared.remove(receiver);
        }
        if declared.len() != member.params.len() {
            return None;
        }
        member.declared_params = Some(declared.into_boxed_slice());
        member.set_is_member_extension(false);
        if let Some(signature) = &mut member.generic_sig {
            signature.receiver = None;
        }
        Some(member)
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
        if let Some(signature) = &mut constructor.generic_sig {
            let captured = &classifier.type_params()[classifier.own_type_parameter_count..];
            let mut ordinal = 0;
            signature.formal_bounds.retain(|_| {
                let keep = !captured.contains(&signature.formals[ordinal]);
                ordinal += 1;
                keep
            });
            signature
                .formals
                .retain(|formal| !captured.contains(formal));
            // Source declarations may publish an own-only constructor result. Its instance type
            // always includes the declaration's captured slots before receiver specialization.
            signature.ret = classifier_instance_type(inner, classifier);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        CompilerIntrinsic, InlineBodyPlan, InlineBodyValue, InlineKind, MemberRealization,
    };

    #[test]
    fn an_inner_constructor_keeps_independent_formals_and_a_complete_instance_result() {
        let inner = crate::types::type_name("fixtures/Outer$Inner");
        let mut classifier = LibraryType::declaration_header();
        classifier.outer_instance = Some(crate::types::type_name("fixtures/Outer"));
        classifier.own_type_parameter_count = 1;
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        classifier.type_parameters = crate::types::TypeParameters::invariant(
            vec!["inner:T".into(), "outer:T".into()],
            vec![vec![any], vec![any]],
        );
        let mut constructor = LibraryMember::new("<init>".into(), vec![], Ty::Unit, "()V".into());
        constructor.generic_sig = Some(GenericSig {
            formals: vec!["inner:T".into(), "outer:T".into(), "constructor:T".into()],
            formal_bounds: vec![vec![any]; 3],
            receiver: None,
            params: vec![],
            ret: Ty::obj_args_name(inner, &[Ty::ty_param("inner:T", any)]),
            return_policy: GenericReturnPolicy::Exact,
        });
        let candidate = FunctionInfo::receiver_bound_constructor(inner, &classifier, constructor)
            .expect("inner candidate");
        let signature = candidate.generic_sig.expect("generic constructor");
        assert_eq!(signature.formals, ["inner:T", "constructor:T"]);
        assert_eq!(signature.formal_bounds, vec![vec![any]; 2]);
        assert_eq!(
            signature.ret.type_args(),
            [Ty::ty_param("inner:T", any), Ty::ty_param("outer:T", any),]
        );
    }

    #[test]
    fn classifier_member_round_trip_keeps_source_and_physical_names_separate() {
        let mut declaration = LibraryMember::new(
            "sourceName".to_string(),
            vec![Ty::Int],
            Ty::Int,
            "(I)I".to_string(),
        );
        declaration.physical_name = Some("platformName".to_string());

        let candidate = FunctionInfo::classifier_member(
            FnKind::Member,
            crate::types::type_name("fixture/Owner"),
            declaration,
        );
        assert_eq!(candidate.callable.name, "sourceName");
        assert_eq!(candidate.callable.physical_name(), "platformName");
        assert_eq!(
            candidate.callable.reflection_name.as_deref(),
            Some("sourceName")
        );
        let selected = candidate.member_with_return(Ty::Int);

        assert_eq!(selected.name, "sourceName");
        assert_eq!(selected.physical_name.as_deref(), Some("platformName"));
    }

    #[test]
    fn constructor_member_round_trip_keeps_declaration_parameters_after_specialization() {
        let outer = crate::types::type_name("fixtures/Outer");
        let inner = crate::types::type_name("fixtures/Outer$Inner");
        let token = Ty::obj("fixtures/Token");
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let formal = Ty::ty_param("outer:O", any);
        let mut classifier = LibraryType::declaration_header();
        classifier.outer_instance = Some(outer);
        classifier.own_type_parameter_count = 0;
        classifier.type_parameters =
            crate::types::TypeParameters::invariant(vec!["outer:O".into()], vec![vec![any]]);
        let mut constructor = LibraryMember::new(
            "<init>".into(),
            vec![formal],
            Ty::Unit,
            "(Lfixtures/Outer;Ljava/lang/Object;)V".into(),
        );
        let physical = vec![Ty::obj_name(outer), Ty::obj("kotlin/Any")];
        constructor.physical_params = physical.clone();
        let mut candidate =
            FunctionInfo::receiver_bound_constructor(inner, &classifier, constructor)
                .expect("a receiver-bound constructor");
        assert_eq!(
            candidate.callable.declared_params.as_deref(),
            Some(&[formal][..])
        );

        // Receiver specialization changes applicability while the declaration remains generic.
        candidate.callable.params = vec![token];
        candidate
            .generic_sig
            .as_mut()
            .expect("constructor signature")
            .params = vec![token];
        let selected = candidate.member_with_return(Ty::obj_args_name(inner, &[token]));
        assert_eq!(selected.params, [token]);
        assert_eq!(selected.constructor_declaration_params(), [formal]);
        assert_eq!(selected.physical_params, physical);

        let callable = LibraryCallable::constructor(inner, &selected);
        assert_eq!(callable.declared_params.as_deref(), Some(&[formal][..]));
        let resolved = crate::symbol_resolver::ResolvedMember::from_callable(
            Ty::obj_args_name(outer, &[token]),
            callable,
            false,
        );
        assert_eq!(resolved.member.constructor_declaration_params(), [formal]);
        assert_eq!(resolved.member.params, [token]);
        assert_eq!(resolved.physical_params, physical);

        let round_trip = FunctionInfo::classifier_member(FnKind::Member, inner, resolved.member);
        assert_eq!(
            round_trip.callable.declared_params.as_deref(),
            Some(&[formal][..])
        );
        assert_eq!(round_trip.callable.params, [token]);
    }

    #[test]
    fn member_projection_keeps_declared_context_receiver_and_value_parameter_order() {
        let owner = crate::types::type_name("fixtures/Owner");
        let context = Ty::obj("fixtures/Context");
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let receiver = Ty::ty_param("owner:R", any);
        let value = Ty::ty_param("owner:T", any);
        let token = Ty::obj("fixtures/Token");
        let declared = vec![context, receiver, value];
        let mut member = LibraryMember::new(
            "transform".into(),
            declared.clone(),
            value,
            "(Lfixtures/Context;Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
        );
        member.context_count = 1;
        member.generic_sig = Some(GenericSig {
            formals: vec![],
            formal_bounds: vec![],
            receiver: Some(receiver),
            params: vec![context, value],
            ret: value,
            return_policy: GenericReturnPolicy::Exact,
        });
        let mut candidate = FunctionInfo::classifier_member(FnKind::Extension, owner, member);
        assert_eq!(
            candidate.callable.declared_params.as_deref(),
            Some(declared.as_slice())
        );
        candidate.callable.params = vec![context, Ty::String, token];
        let signature = candidate.generic_sig.as_mut().expect("member signature");
        signature.receiver = Some(Ty::String);
        signature.params = vec![context, token];
        signature.ret = token;

        let selected = candidate.member_with_return(token);
        assert_eq!(selected.params, [context, token]);
        let projected = crate::libraries::member_record(&candidate);
        assert_eq!(projected.params, [context, Ty::String, token]);
        let round_trip = FunctionInfo::classifier_member(FnKind::Extension, owner, projected);
        assert_eq!(
            round_trip.callable.declared_params.as_deref(),
            Some(declared.as_slice())
        );
        assert_eq!(round_trip.callable.params, [context, Ty::String, token]);
    }

    #[test]
    fn classifier_member_round_trip_keeps_the_exact_compiler_intrinsic() {
        let owner = crate::types::type_name("fixture/Owner");
        let mut declaration = LibraryMember::new(
            "read".to_string(),
            vec![Ty::Int],
            Ty::Char,
            "(I)C".to_string(),
        );
        declaration.realization = MemberRealization::Intrinsic(CompilerIntrinsic::StringGet);

        let candidate = FunctionInfo::classifier_member(FnKind::Member, owner, declaration);
        assert_eq!(
            candidate.callable.compiler_intrinsic,
            Some(CompilerIntrinsic::StringGet)
        );
        assert_eq!(
            candidate.member_with_return(Ty::Char).realization,
            MemberRealization::Intrinsic(CompilerIntrinsic::StringGet)
        );
    }

    #[test]
    fn classifier_member_round_trip_keeps_the_inline_body_plan() {
        let owner = crate::types::type_name("fixture/Owner");
        let mut declaration = LibraryMember::new(
            "test".to_string(),
            vec![Ty::nullable(Ty::obj("kotlin/Any"))],
            Ty::Boolean,
            "(Ljava/lang/Object;)Z".to_string(),
        );
        declaration.inline = InlineKind::MustInline;
        declaration.reified = true;
        declaration.inline_body_plan = Some(Box::new(InlineBodyPlan::ReifiedTypeTest {
            value: InlineBodyValue::Parameter(0),
            type_parameter: 1,
            negated: true,
            defaults: Vec::new(),
        }));

        let candidate = FunctionInfo::classifier_member(FnKind::Member, owner, declaration);
        assert!(matches!(
            candidate.callable.inline_body_plan.as_deref(),
            Some(InlineBodyPlan::ReifiedTypeTest {
                value: InlineBodyValue::Parameter(0),
                type_parameter: 1,
                negated: true,
                defaults,
            }) if defaults.is_empty()
        ));

        let selected = candidate.member_with_return(Ty::Boolean);
        assert!(matches!(
            selected.inline_body_plan.as_deref(),
            Some(InlineBodyPlan::ReifiedTypeTest {
                value: InlineBodyValue::Parameter(0),
                type_parameter: 1,
                negated: true,
                defaults,
            }) if defaults.is_empty()
        ));

        let round_trip = FunctionInfo::classifier_member(FnKind::Member, owner, selected);
        assert!(matches!(
            round_trip.callable.inline_body_plan.as_deref(),
            Some(InlineBodyPlan::ReifiedTypeTest {
                value: InlineBodyValue::Parameter(0),
                type_parameter: 1,
                negated: true,
                defaults,
            }) if defaults.is_empty()
        ));
    }

    #[test]
    fn classifier_conversion_preserves_exact_reified_formals_on_both_candidate_handles() {
        let owner = crate::types::type_name("sample/Owner");
        let mut member =
            LibraryMember::new("mixed".to_string(), Vec::new(), Ty::Unit, "()V".to_string());
        member.call_sig.reified_type_parameter_ordinals = vec![1];
        let candidate = FunctionInfo::classifier_member(FnKind::Member, owner, member);
        assert_eq!(candidate.call_sig.reified_type_parameter_ordinals, [1]);
        assert_eq!(
            candidate.callable.reified_type_parameter_ordinals.as_ref(),
            [1]
        );
    }

    #[test]
    fn classifier_conversion_preserves_the_provider_semantic_role() {
        let owner = crate::types::type_name("fixture/Callable");
        let mut member = LibraryMember::new(
            "perform".to_string(),
            Vec::new(),
            Ty::Unit,
            "()V".to_string(),
        );
        member.semantic_role = Some(crate::types::SemanticCallRole::KotlinFunctionInvoke);

        let candidate = FunctionInfo::classifier_member(FnKind::Member, owner, member);

        assert_eq!(
            candidate.callable.semantic_role,
            Some(crate::types::SemanticCallRole::KotlinFunctionInvoke)
        );
    }
}
