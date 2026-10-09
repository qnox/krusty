//! Common classifier records for Kotlin builtins without a physical JVM class.

use super::*;

/// Minimal classifier signature for a mapped builtin whose physical JVM class is absent.
pub(super) fn mapped_builtin_signature(internal: TypeName) -> Option<LibraryType> {
    // The owner is the receiver's Kotlin identity; the constant-pool boundary supplies its JVM name.
    let members: &[(&str, &str, Ty)] = if internal.matches("kotlin/String") {
        &[("length", "()I", Ty::Int), ("hashCode", "()I", Ty::Int)]
    } else {
        return None;
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
        is_data: false,
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
        value_declaration: None,
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
        annotation_element_defaults: Vec::new(),
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

impl JvmLibraries {
    pub(super) fn builtin_library_type(&self, internal: TypeName) -> Option<LibraryType> {
        let (kind, access, is_nested, class_access) =
            self.cp.builtin_classifier_shape_name(internal)?;
        let (formals, supertype_templates) = self
            .cp
            .builtin_class_gsig_name(internal)
            .unwrap_or_default();
        let variances = self
            .cp
            .builtin_class_variances_name(internal)
            .unwrap_or_else(|| vec![crate::types::TypeVariance::Invariant; formals.len()]);
        let members = self.builtin_members_for_type_name(internal);
        crate::trace_compiler!(
            "resolve",
            "builtin classifier {} members={:?}",
            internal.render(),
            members
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>()
        );
        crate::trace_compiler!(
            "metadata_companions",
            "builtin classifier {} resolved members={:?}",
            internal,
            members
                .iter()
                .map(|member| (member.name.as_str(), member.params.as_slice(), member.ret))
                .collect::<Vec<_>>()
        );
        let mut classifier = builtin_library_type(
            kind,
            access,
            is_nested,
            self.cp.builtin_supertypes_name(internal),
            members,
            self.cp.builtin_constructors_name(internal),
            BuiltinGenericShape {
                type_params: formals,
                type_param_variances: variances,
                supertype_templates,
            },
        );
        classifier.companion_object = self.cp.builtin_companion_object(internal);
        classifier.constants = std::collections::HashMap::new();
        // The builtin's own modality, as a class file would state it: `kotlin.Throwable` stays
        // open when no JDK supplies `java/lang/Throwable`.
        let is_interface = class_access & crate::jvm::classfile::ACC_INTERFACE != 0;
        classifier.inheritance = crate::libraries::ClassifierInheritance {
            is_abstract: class_access & crate::jvm::classfile::ACC_ABSTRACT != 0,
            is_extensible: class_access & crate::jvm::classfile::ACC_FINAL == 0 && !is_interface,
            has_no_arg_constructor: classifier.constructors.iter().any(|constructor| {
                constructor.params.is_empty()
                    && matches!(
                        constructor.visibility,
                        crate::types::Visibility::Public | crate::types::Visibility::Protected
                    )
            }),
        };
        Some(classifier)
    }
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
        // `.kotlin_builtins` declares no data class.
        is_data: false,
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
        value_declaration: None,
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
        annotation_element_defaults: Vec::new(),
    }
}
