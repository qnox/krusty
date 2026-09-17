//! A decoded declaration as the semantic classifier record the front end resolves against.
//!
//! [`super`] answers what a library declares and what those names mean as types; this assembles the
//! two into the [`LibraryType`] a resolver works with. It is the same assembly the JVM backend's
//! optional-annotation path has always done — generalized out of it, because a klib symbol source
//! needs exactly this and a klib is not the JVM's to read for.
//!
//! What it does NOT decide is representation: no descriptors, no erasure, no carrier widths. A
//! backend adds those to the record it receives.

use std::collections::HashMap;

use crate::libraries::{
    CallSig, ClassifierInheritance, LibraryMember, LibraryType, ParamList, TypeKind,
};
use crate::types::{type_name, Ty, TypeName, TypeParameters};

use super::{builtin_bounds, builtin_ty, BuiltinClass};

/// `Class.flags` MODALITY, bits 4..6: 0 FINAL, 1 OPEN, 2 ABSTRACT, 3 SEALED.
fn modality(flags: u64) -> u64 {
    (flags >> 4) & 0x3
}

/// Whether a declaration of this kind and modality can be inherited from, and whether it is
/// abstract.
///
/// The modality word describes a CLASS's own `open`/`abstract`/`sealed`, so a kind that has no use
/// for it answers from its kind instead. An interface and an annotation are both abstract whatever
/// the word says — but only the interface is EXTENSIBLE: an annotation class cannot be subclassed
/// in Kotlin, which is what the JVM path this was generalized from recorded.
fn inheritance(kind: TypeKind, flags: u64, has_no_arg_constructor: bool) -> ClassifierInheritance {
    let modality = modality(flags);
    ClassifierInheritance {
        is_abstract: matches!(kind, TypeKind::Interface | TypeKind::Annotation)
            || matches!(modality, 2 | 3),
        is_extensible: matches!(kind, TypeKind::Interface) || matches!(modality, 1 | 2 | 3),
        has_no_arg_constructor,
    }
}

/// The classifier record a decoded declaration denotes.
///
/// `internal` is the declaration's own identity. It is a parameter rather than something recovered
/// from the record because a companion object's field type is named relative to its owner
/// (`C` + `Companion` → `C.Companion`), and the decoded declaration carries only the simple name.
pub fn library_type(internal: TypeName, declaration: BuiltinClass) -> LibraryType {
    let bounds = builtin_bounds(&declaration.type_params, &HashMap::new());
    let type_parameters = TypeParameters::new(
        declaration
            .type_params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        declaration
            .type_params
            .iter()
            .map(|parameter| {
                parameter
                    .bounds
                    .iter()
                    .map(|bound| builtin_ty(bound, &bounds))
                    .collect()
            })
            .collect(),
        declaration
            .type_params
            .iter()
            .map(|parameter| parameter.variance)
            .collect(),
    );
    let supertype_templates = declaration
        .supertype_tys
        .iter()
        .map(|supertype| builtin_ty(supertype, &bounds))
        .collect::<Vec<_>>();
    let supertypes = declaration
        .supertypes
        .iter()
        .map(|supertype| type_name(supertype))
        .collect::<Vec<_>>()
        .into();
    let mut constructors = Vec::new();
    let mut named_parameter_lists = Vec::new();
    for constructor in declaration.constructors {
        let params = constructor
            .params
            .iter()
            .map(|parameter| builtin_ty(parameter, &bounds))
            .collect::<Vec<_>>();
        let mut member = LibraryMember::new(
            "<init>".to_string(),
            params.clone(),
            Ty::Unit,
            String::new(),
        );
        member.visibility = constructor.visibility;
        member.call_sig = CallSig::metadata_member(
            params.len(),
            constructor.param_names.clone(),
            constructor.param_defaults.clone(),
            constructor.vararg,
        );
        constructors.push(member);
        named_parameter_lists.push(ParamList {
            visibility: constructor.visibility,
            names: constructor.param_names,
            defaults: constructor.param_defaults,
            types: params,
            recv_fun: Vec::new(),
            vararg: constructor.vararg,
            annotation: None,
        });
    }
    let has_no_arg_constructor = constructors
        .iter()
        .any(|constructor| constructor.params.is_empty());
    LibraryType {
        access: declaration.visibility.into(),
        is_kotlin: true,
        source_file: None,
        stable_declaration: None,
        is_nested: declaration.is_nested,
        outer_instance: None,
        kind: declaration.kind,
        inheritance: inheritance(declaration.kind, declaration.flags, has_no_arg_constructor),
        supertypes,
        supertype_templates,
        constructors,
        companion_object: declaration.companion_name.as_ref().map(|simple| {
            (
                simple.clone(),
                crate::types::type_name_nested_child(internal, simple),
            )
        }),
        hidden_member_properties: Default::default(),
        declared_callables: HashMap::new(),
        declared_callable_order: Vec::new(),
        members: Vec::new(),
        companion: Vec::new(),
        constants: HashMap::new(),
        sam_eligible: false,
        callable_signature: None,
        callable_signatures: Vec::new(),
        value_underlying: None,
        value_underlying_property: None,
        alias_target: None,
        own_type_parameter_count: type_parameters.type_params.len(),
        type_parameters,
        sealed_subclasses: declaration
            .sealed_subclasses
            .iter()
            .map(|subclass| type_name(subclass))
            .collect::<Vec<_>>()
            .into(),
        enum_entries: declaration.enum_entries,
        enum_entries_accessor: None,
        named_parameter_lists,
        annotations: Vec::new(),
        retention: None,
        annotation_targets: None,
    }
}
