//! Common classifier records for Kotlin builtins without a physical JVM class.

use super::*;

/// Minimal classifier signature for a mapped builtin whose physical JVM class is absent.
pub(super) fn mapped_builtin_signature(internal: &str) -> Option<LibraryType> {
    // The owner is the receiver's Kotlin identity; the constant-pool boundary supplies its JVM name.
    let members: &[(&str, &str, Ty)] = match internal {
        "kotlin/String" => &[("length", "()I", Ty::Int), ("hashCode", "()I", Ty::Int)],
        _ => return None,
    };
    let members = members
        .iter()
        .map(|(name, descriptor, ret)| {
            LibraryMember::new((*name).to_string(), vec![], *ret, (*descriptor).to_string())
        })
        .collect();
    Some(LibraryType {
        is_kotlin: true,
        access: crate::libraries::ClassifierAccess::Public,
        source_file: None,
        stable_declaration: None,
        is_nested: false,
        outer_instance: None,
        kind: crate::libraries::TypeKind::Class,
        inheritance: Default::default(),
        supertypes: TypeNameList::new(),
        supertype_templates: Vec::new(),
        constructors: Vec::new(),
        hidden_member_properties: Default::default(),
        hidden_deprecated_callables: Default::default(),
        declared_callables: std::collections::HashMap::new(),
        declared_callable_order: Vec::new(),
        members,
        companion: Vec::new(),
        constants: Default::default(),
        sam_eligible: false,
        callable_signature: None,
        callable_signatures: Vec::new(),
        companion_object: None,
        qualified_name: None,
        value_underlying: None,
        value_underlying_property: None,
        alias_target: None,
        type_parameters: crate::types::TypeParameters::default(),
        own_type_parameter_count: 0,
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        enum_entries_accessor: None,
        named_parameter_lists: Vec::new(),
        annotations: Vec::new(),
        retention: None,
        annotation_targets: None,
        mapped_collection: None,
    })
}

/// Property/function distinction supplied by Kotlin's mapped built-in signature.
pub(super) fn mapped_builtin_property(internal: TypeName, name: &str) -> bool {
    internal.matches("kotlin/String") && name == "length"
}

pub(super) struct BuiltinGenericShape {
    pub(super) type_params: Vec<String>,
    pub(super) type_param_variances: Vec<crate::types::TypeVariance>,
    pub(super) supertype_templates: Vec<Ty>,
}

/// A classless Kotlin builtin assembled from `.kotlin_builtins` declaration data.
pub(super) fn builtin_library_type(
    kind: crate::libraries::TypeKind,
    access: crate::libraries::ClassifierAccess,
    is_nested: bool,
    supertypes: TypeNameList,
    members: Vec<LibraryMember>,
    constructors: Vec<LibraryMember>,
    generic: BuiltinGenericShape,
) -> LibraryType {
    let callable_signatures = generic
        .supertype_templates
        .iter()
        .copied()
        .filter(|supertype| matches!(supertype, Ty::Fun(_)))
        .collect::<Vec<_>>();
    let callable_signature = callable_signatures.first().copied();
    LibraryType {
        is_kotlin: true,
        access,
        source_file: None,
        stable_declaration: None,
        is_nested,
        outer_instance: None,
        kind,
        inheritance: Default::default(),
        supertypes,
        supertype_templates: generic.supertype_templates,
        constructors,
        hidden_member_properties: Default::default(),
        hidden_deprecated_callables: Default::default(),
        declared_callables: std::collections::HashMap::new(),
        declared_callable_order: Vec::new(),
        members,
        companion: Vec::new(),
        constants: Default::default(),
        sam_eligible: false,
        callable_signature,
        callable_signatures,
        companion_object: None,
        qualified_name: None,
        value_underlying: None,
        value_underlying_property: None,
        alias_target: None,
        own_type_parameter_count: generic.type_params.len(),
        type_parameters: crate::types::TypeParameters::new(
            generic.type_params.clone(),
            vec![Vec::new(); generic.type_params.len()],
            generic.type_param_variances,
        ),
        sealed_subclasses: TypeNameList::new(),
        enum_entries: Vec::new(),
        enum_entries_accessor: None,
        named_parameter_lists: Vec::new(),
        annotations: Vec::new(),
        retention: None,
        annotation_targets: None,
        mapped_collection: None,
    }
}
