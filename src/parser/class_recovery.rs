//! Recovery value for a class declaration whose nesting guard tripped.

use crate::ast::{ClassDecl, ClassKind};
use crate::diag::Span;
use crate::types::Visibility;

/// An empty final class with a source-impossible name gives later phases nothing to recurse over.
pub(super) fn error_class_decl(span: Span) -> ClassDecl {
    ClassDecl {
        name_span: span,
        primary_ctor_visibility: Visibility::Public,
        name: "<error>".to_string(),
        visibility: Visibility::Public,
        annotations: Vec::new(),
        annotation_args: Vec::new(),
        type_parameters: crate::ast::ClassTypeParameters::new(Vec::new(), Vec::new(), Vec::new()),
        context_params: Vec::new(),
        lexical_type_parameter_captures: Vec::new(),
        props: Vec::new(),
        methods: Vec::new(),
        companion: None,
        body_props: Vec::new(),
        init_order: Vec::new(),
        is_data: false,
        is_value: false,
        value_modifier_span: None,
        primary_constructor_parameters_span: None,
        kind: ClassKind::Class,
        singleton: false,
        enum_entries: Vec::new(),
        is_fun_interface: false,
        modality: crate::ast::Modality::Final,
        final_modifier: false,
        inner_of: None,
        supertypes: Vec::new(),
        interface_delegations: Vec::new(),
        base_class: None,
        base_class_span: None,
        base_type_args: Vec::new(),
        base_args: Vec::new(),
        primary_ctor_annotations: Some(Vec::new()),
        primary_ctor_annotation_args: Vec::new(),
        secondary_ctors: Vec::new(),
        type_aliases: Vec::new(),
        span,
        ctor_close_line: 0,
        companion_block_members: Vec::new(),
        decl_line: 0,
        decl_start_line: 0,
        decl_end_line: 0,
    }
}
