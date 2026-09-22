//! Declared property shape rules.
//!
//! These checks reject a property whose written form cannot exist at its declaration site — a
//! context property with an initializer, a top-level property with an explicit backing field, and
//! the rest. They read syntax and emit diagnostics; they bind no names and produce no signature.

use super::*;

pub(in crate::resolve) fn validate_context_property(
    property: &PropDecl,
    abstract_allowed: bool,
    diags: &mut DiagSink,
) {
    validate_context_property_shape(
        property.span,
        !property.context_params.is_empty(),
        property.is_var,
        property.getter.is_some(),
        property
            .setter
            .as_ref()
            .is_some_and(|setter| setter.body.is_some()),
        property.init.is_some(),
        property.delegate.is_some(),
        property.is_lateinit,
        property.is_const,
        property.getter_reads_field,
        abstract_allowed,
        diags,
    );
}

#[allow(clippy::too_many_arguments)]
pub(in crate::resolve) fn validate_context_property_shape(
    span: Span,
    has_context_parameters: bool,
    mutable: bool,
    getter_has_body: bool,
    setter_has_body: bool,
    has_initializer: bool,
    delegated: bool,
    lateinit: bool,
    is_const: bool,
    getter_reads_field: bool,
    abstract_allowed: bool,
    diags: &mut DiagSink,
) {
    if !has_context_parameters {
        return;
    }
    let default_accessor = !abstract_allowed && (!getter_has_body || mutable && !setter_has_body);
    if has_initializer
        || delegated
        || lateinit
        || is_const
        || getter_reads_field
        || default_accessor
    {
        diags.error(
            span,
            "context property cannot have a backing field".to_string(),
        );
    }
}

/// Declaration-context validity for a syntactically complete property. These checks deliberately
/// live after parsing: the same absent initializer is valid on abstract/expect/external/lateinit
/// declarations and on members definitely assigned by a constructor.
pub(in crate::resolve) fn validate_top_level_property(property: &PropDecl, diags: &mut DiagSink) {
    validate_top_level_property_shape(
        property.span,
        property.receiver.is_some(),
        property.is_companion_extension,
        property.init.is_some(),
        property.delegate.is_some(),
        property.getter.is_some(),
        property.is_lateinit,
        property.is_abstract,
        property.is_external,
        property.is_expect,
        diags,
    );
}

#[allow(clippy::too_many_arguments)]
pub(in crate::resolve) fn validate_top_level_property_shape(
    span: Span,
    has_receiver: bool,
    companion_extension: bool,
    has_initializer: bool,
    delegated: bool,
    getter_has_body: bool,
    lateinit: bool,
    is_abstract: bool,
    is_external: bool,
    is_expect: bool,
    diags: &mut DiagSink,
) {
    if companion_extension && !has_receiver {
        diags.error(
            span,
            "companion extension property must declare a classifier receiver".to_string(),
        );
    }
    if has_receiver && has_initializer && !companion_extension {
        diags.error(
            span,
            "extension property cannot have a backing field".to_string(),
        );
    }
    let has_value = has_initializer || delegated || getter_has_body;
    if !has_value && !lateinit && !is_abstract && !is_external && !is_expect {
        diags.error(span, "property must be initialized or be abstract");
    }
}

pub(in crate::resolve) fn validate_member_property(
    property: &PropDecl,
    owner_is_interface: bool,
    diags: &mut DiagSink,
) {
    validate_member_property_shape(
        property.span,
        owner_is_interface,
        property.getter_declared,
        property.getter.is_some(),
        property.init.is_some(),
        property.delegate.is_some(),
        property.is_lateinit,
        property.is_abstract,
        property.is_external,
        property.is_expect,
        diags,
    );
}

#[allow(clippy::too_many_arguments)]
pub(in crate::resolve) fn validate_member_property_shape(
    span: Span,
    owner_is_interface: bool,
    getter_declared: bool,
    getter_has_body: bool,
    has_initializer: bool,
    delegated: bool,
    lateinit: bool,
    is_abstract: bool,
    is_external: bool,
    is_expect: bool,
    diags: &mut DiagSink,
) {
    if owner_is_interface && has_initializer {
        diags.error(span, "property initializers are not allowed in interfaces");
    }
    // A written default getter supplies no value of its own. Unlike an entirely accessor-less
    // member, it cannot be a deferred constructor assignment. Keep this distinction in the AST and
    // diagnose it here; general definite assignment remains the constructor-flow checker's job.
    if getter_declared
        && !getter_has_body
        && !has_initializer
        && !delegated
        && !owner_is_interface
        && !lateinit
        && !is_abstract
        && !is_external
        && !is_expect
    {
        diags.error(span, "property must be initialized or be abstract");
    }
}

pub(in crate::resolve) fn validate_explicit_backing_field(
    property: &PropDecl,
    diags: &mut DiagSink,
) {
    validate_explicit_backing_field_shape(
        property.span,
        property.explicit_backing_field.is_some(),
        property.is_var,
        property.is_open,
        property.getter.is_some(),
        property.setter.is_some(),
        property.delegate.is_some(),
        property.is_const,
        property.receiver.is_some(),
        !property.context_params.is_empty(),
        diags,
    );
}

#[allow(clippy::too_many_arguments)]
pub(in crate::resolve) fn validate_explicit_backing_field_shape(
    span: Span,
    explicit_backing_field: bool,
    mutable: bool,
    is_open: bool,
    custom_getter: bool,
    custom_setter: bool,
    delegated: bool,
    is_const: bool,
    has_receiver: bool,
    has_context_parameters: bool,
    diags: &mut DiagSink,
) {
    if explicit_backing_field
        && (mutable
            || is_open
            || custom_getter
            || custom_setter
            || delegated
            || is_const
            || has_receiver
            || has_context_parameters)
    {
        diags.error(
            span,
            "an explicit backing field requires a final, read-only property with default accessors",
        );
    }
}
