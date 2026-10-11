//! Members a compiler plugin contributes to a class's companion object.
//!
//! The serialization plugin declares `serializer()` on the companion of every serializable class.
//! A hand-written companion receives the member; a class without one gets a synthesized `Companion`
//! object that holds only the contributed members.

use crate::resolve::*;

/// The companion members one class contributes, recorded while its declaration is collected.
pub(super) struct ContributedCompanion {
    pub(super) internal: TypeName,
    pub(super) source_file: u32,
    pub(super) order: Vec<String>,
    pub(super) methods: MethodMap,
}

/// Install every contribution once the whole source set is collected, so a hand-written companion
/// declared after its owner already has its own signature to extend.
pub(super) fn install_contributed_companions(
    table: &mut SymbolTable,
    contributions: Vec<ContributedCompanion>,
) {
    for contribution in contributions {
        if let Some(companion) = table.classes.get_mut(&contribution.internal) {
            extend_companion(companion, contribution);
        } else {
            table.insert_class_sig(contribution.internal, synthesized_companion(contribution));
        }
    }
}

fn extend_companion(companion: &mut ClassSig, mut contribution: ContributedCompanion) {
    for name in contribution.order {
        let signatures = contribution
            .methods
            .remove(&name)
            .expect("a contributed companion name must retain its overloads");
        if !companion.declared_callable_order.contains(&name) {
            companion.declared_callable_order.push(name.clone());
        }
        companion
            .methods
            .entry(name)
            .or_default()
            .extend(signatures);
    }
}

fn synthesized_companion(contribution: ContributedCompanion) -> ClassSig {
    ClassSig {
        internal: contribution.internal,
        stable_declaration: None,
        source_file: contribution.source_file,
        source_decl: None,
        name_span: crate::diag::Span::new(0, 0),
        is_nested: true,
        visibility: Visibility::Public,
        annotations: Vec::new(),
        applied_annotations: Vec::new(),
        annotation_class_arguments: Vec::new(),
        generated_nested_classifiers: Vec::new(),
        props: Vec::new(),
        declared_props: HashMap::new(),
        contextual_props: HashMap::new(),
        constants: HashMap::new(),
        member_ext_props: HashMap::new(),
        member_ext_funs: HashMap::new(),
        has_primary_ctor: true,
        primary_constructor_declaration: None,
        primary_constructor_annotations: Vec::new(),
        ctor_params: Vec::new(),
        ctor_param_shapes: Vec::new(),
        methods: contribution.methods,
        declared_callable_order: contribution.order,
        source_methods: Vec::new(),
        flags: ClassFlags::default().with_final(true).with_object(true),
        inner_of: None,
        companion_internal: None,
        lateinit_props: Default::default(),
        interfaces: Default::default(),
        interface_type_args: Vec::new(),
        delegated_interfaces: Vec::new(),
        callable_signature: None,
        callable_signatures: Vec::new(),
        super_internal: None,
        interfaces_before_superclass: 0,
        super_type_args: Vec::new(),
        super_ctor_params: Vec::new(),
        ctor_param_names: Vec::new(),
        ctor_implicit_integer_coercion: Vec::new(),
        ctor_vararg: None,
        ctor_defaults: Vec::new(),
        secondary_ctors: Vec::new(),
        secondary_ctor_shapes: Vec::new(),
        secondary_ctor_call_sigs: Vec::new(),
        secondary_constructor_declarations: Vec::new(),
        secondary_constructor_annotations: Vec::new(),
        type_parameters: crate::types::TypeParameters::default(),
        type_parameter_extra_bounds: Vec::new(),
        captured_type_parameters: crate::types::TypeParameters::default(),
        metadata_captured_type_parameters: Vec::new(),
        generic_props: HashMap::new(),
        nullable_tparam_props: HashMap::new(),
        generic_property_shapes: HashMap::new(),
        value_field: None,
        full_value: false,
        generic_methods: HashMap::new(),
    }
}
