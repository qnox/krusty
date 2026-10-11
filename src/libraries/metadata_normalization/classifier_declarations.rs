//! Kotlin classes normalized into the common classifier model.
//!
//! A class's shape (kind, modality, type parameters, direct supertypes, companion, enum entries)
//! comes from its metadata alone. Its members and constructors are ordinary normalized callables;
//! this module adds the classifier-level records the common model keeps beside them. Nothing here
//! assigns a provider identity, and nothing walks inheritance: core owns the hierarchy.

use super::{type_signatures::EnclosingBounds, TypeParameterIdentities};
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::function_classifiers::supertype_classifier;
use crate::libraries::{
    constructor_generic_signature, CallSig, ClassifierInheritance, FnKind, FunctionInfo,
    ImplicitClassifierCallable, LibraryMember, LibraryType, ParamList, TypeKind,
};
use crate::metadata::semantic::{
    semantic_bounds_with_identities, semantic_ty_with_identities, AnnotationArgument, KotlinClass,
    KotlinConstructor, KotlinModality,
};
use crate::types::{type_name, Ty, TypeName, TypeParameters, TypeVariance};

/// A class's metadata that does not describe one consistent declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InconsistentClassifier {
    detail: String,
}

impl std::fmt::Display for InconsistentClassifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

/// The type parameters a class puts in scope: its own, then, for an `inner` class, every one its
/// enclosing class puts in scope.
#[derive(Clone, Debug, Default)]
pub(crate) struct ClassTypeParameters {
    names: Vec<String>,
    bounds: Vec<Vec<Ty>>,
    variances: Vec<TypeVariance>,
    own: usize,
    identities: TypeParameterIdentities,
    /// Primary upper bounds of all of them by metadata identity: the scope the class's own member
    /// declarations are read in.
    enclosing: EnclosingBounds,
}

impl ClassTypeParameters {
    /// The scope of `class`; `captured` is the enclosing class's scope when `class` is `inner`.
    pub(crate) fn of(
        class: &KotlinClass,
        identities: TypeParameterIdentities,
        captured: Option<&Self>,
    ) -> Self {
        let outer = captured
            .map(|scope| scope.enclosing.clone())
            .unwrap_or_default();
        let enclosing =
            semantic_bounds_with_identities(&class.type_params, &outer, identities.by_id());
        let mut scope = Self {
            names: identities.formals().to_vec(),
            bounds: class
                .type_params
                .iter()
                .map(|parameter| {
                    parameter
                        .bounds
                        .iter()
                        .map(|bound| {
                            semantic_ty_with_identities(bound, &enclosing, identities.by_id())
                        })
                        .collect()
                })
                .collect(),
            variances: class
                .type_params
                .iter()
                .map(|parameter| parameter.variance)
                .collect(),
            own: class.type_params.len(),
            identities,
            enclosing,
        };
        if let Some(captured) = captured {
            scope.names.extend(captured.names.iter().cloned());
            scope.bounds.extend(captured.bounds.iter().cloned());
            scope.variances.extend(captured.variances.iter().copied());
        }
        scope
    }

    /// The bounds the class's member declarations are read with.
    pub(crate) fn enclosing(&self) -> &EnclosingBounds {
        &self.enclosing
    }

    /// Declaration-owned identities of this class's parameters and every captured outer
    /// parameter, for normalizing its constructors and members.
    pub(crate) fn identities(&self) -> &TypeParameterIdentities {
        &self.identities
    }
}

/// Normalize the shape of `class`, whose identity is `identity` and whose metadata name is
/// `metadata_name` (`lib/Outer.Nested`): everything but its callables, which a provider adds once
/// it has assigned their identities. `outer_instance` is the enclosing class an `inner` class
/// captures.
pub(crate) fn classifier_shape(
    identity: TypeName,
    metadata_name: &str,
    class: &KotlinClass,
    type_parameters: &ClassTypeParameters,
    outer_instance: Option<TypeName>,
) -> Result<LibraryType, InconsistentClassifier> {
    let bounds = type_parameters.enclosing();
    let kind = class.kind;
    let mut supertypes = Vec::with_capacity(class.supertype_tys.len());
    let mut supertype_templates = Vec::with_capacity(class.supertype_tys.len());
    let mut callable_signatures = Vec::new();
    for declared in &class.supertype_tys {
        let supertype =
            semantic_ty_with_identities(declared, bounds, type_parameters.identities().by_id());
        // A function supertype keeps its callable shape and is also an edge to the function
        // classifier it instantiates, which declares the `invoke` the class overrides.
        if matches!(supertype, Ty::Fun(_)) {
            callable_signatures.push(supertype);
        }
        let template = supertype_classifier(supertype);
        let edge = template
            .obj_internal()
            .ok_or_else(|| InconsistentClassifier {
                detail: format!(
                    "supertype {} of {} has no classifier identity",
                    declared.render(),
                    identity.render()
                ),
            })?;
        supertypes.push(edge);
        supertype_templates.push(template);
    }
    let own_parameters = type_parameters.names[..type_parameters.own]
        .iter()
        .zip(&type_parameters.bounds)
        .map(|(name, bounds)| {
            Ty::ty_param(
                name,
                bounds
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
            )
        })
        .collect::<Vec<_>>();
    let value_declaration = class
        .inline_class_property
        .as_deref()
        .map(|property| {
            let declared = class
                .properties
                .iter()
                .find(|candidate| candidate.name == property)
                .ok_or_else(|| InconsistentClassifier {
                    detail: format!(
                        "value class {} declares no underlying property {property}",
                        identity.render()
                    ),
                })?;
            Ok(crate::types::DeclaredValueClass {
                property: property.into(),
                underlying: semantic_ty_with_identities(
                    &declared.ty,
                    bounds,
                    type_parameters.identities().by_id(),
                ),
                type_parameters: own_parameters.clone().into_boxed_slice(),
            })
        })
        .transpose()?;
    let is_interface = matches!(kind, TypeKind::Interface | TypeKind::Annotation);
    Ok(LibraryType {
        access: class.visibility.into(),
        is_kotlin: true,
        source_file: None,
        stable_declaration: None,
        is_nested: class.is_nested,
        outer_instance,
        kind,
        inheritance: ClassifierInheritance {
            is_abstract: is_interface || class.modality.is_abstract(),
            is_extensible: !is_interface && class.modality != KotlinModality::Final,
            // Settled by the provider once it has published the constructors.
            has_no_arg_constructor: false,
        },
        supertypes: supertypes.into(),
        supertype_templates,
        constructors: Vec::new(),
        hidden_member_properties: Default::default(),
        hidden_deprecated_callables: Default::default(),
        declared_callables: Default::default(),
        declared_callable_order: Vec::new(),
        members: Vec::new(),
        companion: Vec::new(),
        constants: Default::default(),
        sam_eligible: class.is_fun_interface,
        callable_signature: callable_signatures.first().copied(),
        callable_signatures,
        // The metadata name separates packages with `/` and classes with `.`.
        qualified_name: Some(metadata_name.replace('/', ".").into()),
        companion_object: class.companion_name.as_ref().map(|name| {
            (
                name.clone(),
                crate::types::type_name_nested_child(identity, name),
            )
        }),
        value_underlying: value_declaration
            .as_ref()
            .map(|declaration| declaration.underlying),
        value_declaration,
        alias_target: None,
        own_type_parameter_count: type_parameters.own,
        type_parameters: TypeParameters::new(
            type_parameters.names.clone(),
            type_parameters.bounds.clone(),
            type_parameters.variances.clone(),
        ),
        sealed_subclasses: class
            .sealed_subclasses
            .iter()
            .map(|subclass| type_name(subclass))
            .collect::<Vec<_>>()
            .into(),
        enum_entries: class.enum_entries.clone(),
        enum_entries_accessor: None,
        named_parameter_lists: Vec::new(),
        // KLIB annotation values are not yet fully decoded into the common typed application
        // contract. Publishing the identity with fabricated empty arguments is semantically false;
        // retention/target policy below continues to consume its narrow decoded enum facts.
        annotations: Vec::new(),
        retention: (kind == TypeKind::Annotation).then(|| declared_retention(class).to_string()),
        annotation_targets: (kind == TypeKind::Annotation).then(|| declared_targets(class)),
        mapped_collection: None,
        annotation_element_defaults: Vec::new(),
    })
}

/// Settle whether a class of `modality` can be constructed with no arguments from the
/// constructors a provider published on it. A sealed class is never constructed directly.
pub(crate) fn settle_no_arg_construction(shape: &mut LibraryType, modality: KotlinModality) {
    shape.inheritance.has_no_arg_constructor = modality != KotlinModality::Sealed
        && shape
            .constructors
            .iter()
            .any(|constructor| constructor.call_sig.required == 0);
}

/// Normalize one constructor of the class `shape` describes. Its parameters are read in the
/// class's scope; a generic class's constructor infers the class's type arguments.
pub(crate) fn declared_constructor(
    identity: TypeName,
    shape: &LibraryType,
    type_parameters: &ClassTypeParameters,
    constructor: &KotlinConstructor,
    parameters: &[ResolvedParameterIdentity],
) -> LibraryMember {
    let params = constructor
        .params
        .iter()
        .map(|parameter| {
            semantic_ty_with_identities(
                parameter,
                type_parameters.enclosing(),
                type_parameters.identities().by_id(),
            )
        })
        .collect::<Vec<_>>();
    let mut member = LibraryMember::new("<init>".to_string(), params, Ty::Unit, String::new());
    member.owner = Some(identity);
    member.visibility = constructor.visibility;
    member.set_is_primary_constructor(constructor.is_primary);
    member.call_sig = CallSig::metadata_member(
        member.params.len(),
        constructor.param_names.clone(),
        constructor.param_defaults.clone(),
        constructor.vararg,
    );
    member.call_sig.parameter_identities = parameters.to_vec();
    member.generic_sig = constructor_generic_signature(identity, shape, &member);
    member
}

/// The named-argument parameter list of a normalized constructor.
pub(crate) fn constructor_parameter_list(constructor: &LibraryMember) -> ParamList {
    let names = &constructor.call_sig.param_names;
    // A call signature omits its defaults when no parameter declares one; a parameter list
    // states each parameter's.
    let defaults = if constructor.call_sig.param_defaults.is_empty() {
        vec![false; names.len()]
    } else {
        constructor.call_sig.param_defaults.clone()
    };
    ParamList {
        visibility: constructor.visibility,
        names: names.clone(),
        defaults,
        types: constructor.params.clone(),
        recv_fun: constructor.call_sig.lambda_receiver_params.clone(),
        vararg: constructor.call_sig.vararg_index,
        annotation: None,
    }
}

/// The classifier-level record of a normalized function: an instance member's entry in
/// [`LibraryType::members`], or an associated function's entry in [`LibraryType::companion`].
/// It carries the function's provider identity, so selecting the declaration through either view
/// realizes the same declaration.
pub(crate) fn member_record(function: &FunctionInfo) -> LibraryMember {
    // Use the common complete projection so the classifier view cannot silently lose contracts,
    // return-value policy, provider identity, or any future callable fact.
    let mut member = function.member_with_return(function.callable.ret);
    member.set_ret_nullable(function.callable.ret.is_nullable());
    member.set_is_member_extension(function.kind == FnKind::Extension);
    member.associated_classifier = function.associated_classifier;
    member.associated_access_owner = function.associated_access_owner;
    member
}

/// The `values(): Array<E>` every enum class `owner` declares implicitly.
pub(crate) fn enum_values(owner: TypeName) -> LibraryMember {
    let mut values = LibraryMember::new(
        "values".to_string(),
        Vec::new(),
        Ty::array(Ty::obj_name(owner)),
        String::new(),
    );
    values.owner = Some(owner);
    values.implicit_classifier_callable = Some(ImplicitClassifierCallable::EnumValues);
    values
}

/// The `valueOf(value: String): E` every enum class `owner` declares implicitly.
pub(crate) fn enum_value_of(owner: TypeName) -> LibraryMember {
    let mut value_of = LibraryMember::new(
        "valueOf".to_string(),
        vec![Ty::String],
        Ty::obj_name(owner),
        String::new(),
    );
    value_of.owner = Some(owner);
    value_of.implicit_classifier_callable = Some(ImplicitClassifierCallable::EnumValueOf);
    value_of.call_sig = CallSig::metadata_member(1, vec!["value".to_string()], Vec::new(), None);
    value_of
}

/// The getter of the `entries: EnumEntries<E>` property an enum class `owner` declares implicitly,
/// under the accessor name its artifact gives it. The getter is not a source-callable function;
/// only the synthetic `EnumType.entries` property selects it.
pub(crate) fn enum_entries_getter(owner: TypeName, name: &str) -> LibraryMember {
    let mut getter = LibraryMember::new(
        name.to_string(),
        Vec::new(),
        Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj_name(owner)]),
        String::new(),
    );
    getter.owner = Some(owner);
    getter
}

/// The class's `@kotlin.annotation.Retention`, as the common model's retention name. Kotlin's
/// default is `RUNTIME`; `BINARY` is the `CLASS` policy. Only an entry of
/// `kotlin.annotation.AnnotationRetention` itself is a retention.
pub(crate) fn declared_retention(declaration: &KotlinClass) -> &'static str {
    let retention = type_name("kotlin/annotation/Retention");
    let entry = declaration
        .annotations
        .iter()
        .find(|annotation| annotation.identity == retention)
        .and_then(|annotation| annotation.argument("value"));
    let declared = match entry {
        Some(AnnotationArgument::Enum { class, entry }) => {
            crate::types::AnnotationRetention::of_entry(*class, entry)
        }
        _ => None,
    };
    match declared {
        Some(crate::types::AnnotationRetention::Source) => "SOURCE",
        Some(crate::types::AnnotationRetention::Binary) => "CLASS",
        _ => "RUNTIME",
    }
}

/// Where an unprefixed application may land, from the class's `@kotlin.annotation.Target`; a class
/// that declares none is applicable everywhere.
pub(crate) fn declared_targets(declaration: &KotlinClass) -> crate::types::AnnotationTargets {
    let target = type_name("kotlin/annotation/Target");
    let Some(allowed) = declaration
        .annotations
        .iter()
        .find(|annotation| annotation.identity == target)
        .and_then(|annotation| annotation.argument("allowedTargets"))
    else {
        return crate::types::AnnotationTargets::DEFAULT;
    };
    let entries = match allowed {
        AnnotationArgument::Array(elements) => elements.as_slice(),
        single => std::slice::from_ref(single),
    };
    crate::types::AnnotationTargets::kotlin(entries.iter().filter_map(|element| match element {
        AnnotationArgument::Enum { class, entry } => {
            crate::types::KotlinTarget::of_entry(*class, entry)
        }
        _ => None,
    }))
}
