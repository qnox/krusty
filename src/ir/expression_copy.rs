//! What a copy of an expression inherits from the expression it copies.
//!
//! Every [`IrFile`] field is classified here, once, by destructuring the file without a rest
//! pattern: a field added to [`IrFile`] does not compile until its author decides whether it is a
//! fact about one expression, and if so whether a copy of that expression inherits it. A field that
//! holds several tables of its own is classified by its owning module's [`InheritedFacts`] impl,
//! which destructures that struct the same way. `deny(unused_variables)` keeps the two lists honest:
//! a field named as inherited must be copied.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;

use super::{ExprId, IrExpr, IrFile};

/// Facts recorded about expressions that a copy of an expression inherits.
pub(super) trait InheritedFacts {
    /// Give `target`, a copy of `source`, the facts this table records about `source`.
    fn inherit(&mut self, source: ExprId, target: ExprId);
}

impl<V: Clone, S: BuildHasher> InheritedFacts for HashMap<ExprId, V, S> {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        if let Some(value) = self.get(&source).cloned() {
            self.insert(target, value);
        }
    }
}

impl<S: BuildHasher> InheritedFacts for HashSet<ExprId, S> {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        if self.contains(&source) {
            self.insert(target);
        }
    }
}

/// Give `target`, a structural copy of `source`, every fact a copy inherits.
#[deny(unused_variables)]
pub(super) fn copy_expression_facts(ir: &mut IrFile, source: ExprId, target: ExprId) {
    let IrFile {
        // Facts about the expression itself. A copy is the same source operation, so it inherits
        // its origin, position, types, checked selections and lowering roles.
        exprs,
        module_constructions,
        callable_reference_provenance,
        callable_reference_enclosures,
        expression_owners,
        generated_secondary_constructor_calls,
        fir_origins,
        checked_return_depths,
        inline_return_frames,
        inline_cleanup_results,
        annotation_constructions,
        property_initializer_stores,
        positions,
        generated_lines,
        value_names,
        debug_local_provenance,
        inline_expansions,
        logical_types,
        deferred_local_types,
        whens,
        binding_read_stability,
        null_guards,
        safe_cast_temporaries,
        lateinit_initialization_probes,
        short_circuits,
        plain_updates,
        postfix_updates,
        negations,
        transparent_loop_bodies,
        for_loop_next_loops,
        written_casts,
        elvis_safe_call_guards,
        jvm_value_class_null_guards,
        physical_types,
        jvm_erased_primitive_wrapper_values,
        compiled_inline_calls,
        module_inline_calls,
        inline_call_sites,
        inline_callable_reference_receiver_bindings,
        inline_regions,
        callable_scopes,
        kept_start_lines,
        builtin_calls,
        safe_calls,
        jvm_intrinsic_calls,
        elvis_temporaries,
        call_operand_bindings,
        argument_evaluation_bindings,
        invocation_operand_bindings,
        destructuring_containers,
        inline_synthetic_lines,
        external_inline_expansions,
        external_frame_lines,
        external_frame_closes,
        module_member_accesses,
        jvm_protected_dependency_calls,
        value_class_constructor_facts,
        value_class_suspend_calls,
        suspend_calls,
        suspend_lambda_placements,
        suspend_call_overridden_results,
        intrinsic_suspension_points,
        erased_value_constructions,
        reified_call_subst,
        reified_markers,
        lambda_exits,
        inline_call_type_arguments,
        ext_call_source_receiver,
        external_singleton_dispatches,
        dispatch_classes,
        call_declared_ret,
        declaration_result_coercions,
        jvm_external_result_coercions,
        declaration_argument_boundaries,
        call_declared_params,
        physical_call_parameters,
        construction_declared_params,
        construction_targets,
        static_extension_receivers,
        call_inline_modifiers,
        synthesized_accessor_calls,
        private_property_accessor_calls,

        // Facts about one expression that a copy does not inherit.
        //
        // The class name a target chose for one source callable reference or lambda; the JVM
        // names each copy itself (`realize_copied_lambda_class_names`).
        callable_reference_names: _,
        // Constructor statements the JVM's in-place delegate realization recorded.
        jvm_in_place_delegates: _,
        // The role the JVM value-class pass gave one physical type operation, and the unbox call
        // it selected as that operation's receiver. Operation relocation moves them with the
        // operation.
        value_class_type_operations: _,
        value_class_unbox_type_operation_edges: _,
        // The accessor bodies materialized for one inline property access.
        inline_property_access: _,
        // A callable reference consumed as an inline argument: the expansion that consumes the
        // argument edge records it.
        inline_callable_reference_arguments: _,
        // Its value is another expression of the adapter body, which a copy does not remap.
        callable_reference_adapter_results: _,
        // The one block a class initializer runs as its `init { … }` body. Whoever places a copy
        // in another initializer marks it there (the serialization plugin's constructors).
        initializer_blocks: _,
        // JVM realization of one physical call: the exact static it calls, its overridden-call
        // plan and its language-member role. Operation relocation moves them with the call.
        jvm_member_targets: _,
        jvm_overridden_call_realizations: _,
        semantic_call_roles: _,
        // Keyed by the property operation identity, which a copy keeps (`IrFile::add_expr`), not
        // by the expression.
        property_declaration_types: _,
        property_selected_accessors: _,
        property_external_accessors: _,
        property_accessor_jvm_realizations: _,

        // The file, its declarations, classes, functions, statics and lifted callables. None is
        // keyed by an expression, so an expression copy leaves them alone.
        package: _,
        source_line_count: _,
        source_debug: _,
        recursive_type_of: _,
        file_annotations: _,
        file_annotations_attached: _,
        functions: _,
        local_delegate_plans: _,
        local_delegate_plan_ids: _,
        jvm_suspend_impl_bodies: _,
        generated_member_publications: _,
        generated_facade_functions: _,
        synthesized_data_class_members: _,
        checked_callable_functions: _,
        plugin_declaration_functions: _,
        referenced_module_callables: _,
        referenced_module_properties: _,
        referenced_module_classifiers: _,
        classifier_hierarchies: _,
        inherited_defaults: _,
        custom_serializer_constructions: _,
        type_use_serializer_constructions: _,
        property_overrides: _,
        function_overrides: _,
        checked_classifier_classes: _,
        local_class_name_provenance: _,
        lambda_enclosures: _,
        lifting_sequences: _,
        lifting_sequence_source_order: _,
        lifted_functions: _,
        lifted_names: _,
        local_function_spans: _,
        lambda_class_names: _,
        declaration_paths: _,
        class_source_qualified_names: _,
        class_source_orders: _,
        class_diagnostic_spans: _,
        checked_interface_delegation_fields: _,
        checked_interface_delegation_initializers: _,
        checked_interface_delegate_calls: _,
        checked_enum_entry_classes: _,
        checked_constructor_bodies: _,
        checked_properties: _,
        local_property_layouts: _,
        checked_class_initializers: _,
        secondary_initializer_value_bases: _,
        checked_enum_entry_bodies: _,
        shared_capture_parameters: _,
        shared_class_capture_fields: _,
        shared_super_capture_parameters: _,
        shared_secondary_super_capture_parameters: _,
        class_capture_identities: _,
        class_capture_order: _,
        anonymous_super_forwards: _,
        class_static_local_functions: _,
        multifile_private_names: _,
        classes: _,
        generated_classes: _,
        generated_functions: _,
        generated_secondary_constructors: _,
        class_type_aliases: _,
        package_functions: _,
        entry_point: _,
        box_entry: _,
        package_properties: _,
        package_type_aliases: _,
        jvm_value_class_secondary_ctors: _,
        statics: _,
        jvm_companion_hoisted_statics: _,
        companion_clinit_bodies: _,
        jvm_static_field_names: _,
        jvm_field_statics: _,
        jvm_companion_property_statics: _,
        jvm_companion_delegate_statics: _,
        function_annotations: _,
        function_source_annotations: _,
        compose: _,
        plugin_extended_signatures: _,
        property_annotation_markers: _,
        jvm_package_property_annotation_markers: _,
        public_abi_annotation_implementations: _,
        annotation_element_defaults: _,
        super_constructor_default_arguments: _,
        external_super_constructors: _,
        external_secondary_super_constructors: _,
        lambda_origins: _,
        specialized_functions: _,
        specialized_expansion_order: _,
        specialized_anonymous_classes: _,
        reified_anonymous_declarations: _,
        runtime_reified_lambda_implementations: _,
        inline_anonymous_lambdas: _,
        class_method_owners: _,
        default_entries: _,
        fn_params: _,
        fn_param_declared_nullable: _,
        fn_declared_spellings: _,
        fn_source_names: _,
        class_declared_spellings: _,
        class_superclass_positions: _,
        prop_declared_spellings: _,
        extension_receiver_fns: _,
        companion_blocks: _,
        class_declarations: _,
        fn_context_counts: _,
        member_ext_props: _,
        type_reflection: _,
        inline_fns: _,
        function_typed_parameter_fns: _,
        operator_fns: _,
        infix_fns: _,
        tailrec_fns: _,
        fn_param_annotations: _,
        accessor_annotations: _,
        package_property_annotations: _,
        companion_block_field_annotations: _,
        fn_param_no_infer: _,
        fn_return_value_statuses: _,
        class_ctor_defaults: _,
        open_methods: _,
        method_visibilities: _,
        function_reference_access_bridges: _,
        must_inline_lambdas: _,
        synthetic_methods: _,
        jvm_value_class_representation_order: _,
        jvm_value_class_constructor_impls: _,
        jvm_value_class_member_signatures: _,
        jvm_nullability_unannotated_methods: _,
        jvm_unrealized_lambda_classes: _,
        jvm_value_class_receiver_impls: _,
        jvm_value_class_generated_any: _,
        jvm_value_class_any_delegators: _,
        bridge_methods: _,
        default_stub_boxed_params: _,
        module_source_value_classes: _,
        module_readable_value_classes: _,
        synthetic_classes: _,
        declarationless_classes: _,
        generated_static_facts: _,
        deprecated_methods: _,
        deprecated_classes: _,
        inline_only_fns: _,
        foreign_inline_templates: _,
        top_level_inline_functions: _,
        suspend_funs: _,
        interface_delegation_forwarders: _,
        unlooped_tailrec: _,
        fresh_method_decls: _,
        suspend_declared_sigs: _,
        vc_declared_sigs: _,
        value_class_renamed_functions: _,
        value_class_mangle_suffixes: _,
        top_level_function_fids: _,
        public_inline_functions: _,
        value_class_suspend_returns: _,
        signatures: _,
        callable_bound_type_parameters: _,
        member_semantic_sigs: _,
        prop_visibilities: _,
        class_visibilities: _,
        ctor_close_lines: _,
        ctor_open_lines: _,
        secondary_ctor_lines: _,
        ctor_visibilities: _,
        fn_varargs: _,
        declared_class_statics: _,
        prop_decl_lines: _,
        fn_decl_lines: _,
        fn_debug_locals: _,
        fn_close_lines: _,
        fn_end_lines: _,
        fn_sig_lines: _,
        fn_signature_offsets: _,
        fn_source_order: _,
        fn_continuation_ordinal: _,
        fn_contracts: _,
        class_signatures: _,
        declared_class_self_types: _,
        lowered_class_signatures: _,
        field_signatures: _,
        external_value_classes: _,
        external_value_class_declarations: _,
        classifier_roles: _,
        serialization_cache_methods: _,
        singleton_extension_unread_receivers: _,
        external_overridden_declarations: _,
        callable_semantic_roles: _,
        lambda_own_params_from: _,
        lambda_inferred_results: _,
        lambda_sam_signature: _,
        lambda_sam_jvm_signature: _,
    } = ir;

    let inherited: [&mut dyn InheritedFacts; 80] = [
        module_constructions,
        callable_reference_provenance,
        callable_reference_enclosures,
        expression_owners,
        generated_secondary_constructor_calls,
        fir_origins,
        checked_return_depths,
        inline_return_frames,
        inline_cleanup_results,
        annotation_constructions,
        property_initializer_stores,
        positions,
        generated_lines,
        value_names,
        debug_local_provenance,
        inline_expansions,
        logical_types,
        deferred_local_types,
        whens,
        binding_read_stability,
        null_guards,
        safe_cast_temporaries,
        lateinit_initialization_probes,
        short_circuits,
        plain_updates,
        postfix_updates,
        negations,
        transparent_loop_bodies,
        for_loop_next_loops,
        written_casts,
        elvis_safe_call_guards,
        jvm_value_class_null_guards,
        physical_types,
        jvm_erased_primitive_wrapper_values,
        compiled_inline_calls,
        module_inline_calls,
        inline_call_sites,
        inline_callable_reference_receiver_bindings,
        inline_regions,
        callable_scopes,
        kept_start_lines,
        builtin_calls,
        safe_calls,
        jvm_intrinsic_calls,
        elvis_temporaries,
        call_operand_bindings,
        argument_evaluation_bindings,
        invocation_operand_bindings,
        destructuring_containers,
        inline_synthetic_lines,
        external_inline_expansions,
        external_frame_lines,
        external_frame_closes,
        module_member_accesses,
        jvm_protected_dependency_calls,
        value_class_constructor_facts,
        value_class_suspend_calls,
        suspend_calls,
        suspend_lambda_placements,
        suspend_call_overridden_results,
        intrinsic_suspension_points,
        erased_value_constructions,
        reified_call_subst,
        reified_markers,
        lambda_exits,
        inline_call_type_arguments,
        ext_call_source_receiver,
        external_singleton_dispatches,
        dispatch_classes,
        call_declared_ret,
        declaration_result_coercions,
        jvm_external_result_coercions,
        call_declared_params,
        physical_call_parameters,
        construction_declared_params,
        construction_targets,
        static_extension_receivers,
        call_inline_modifiers,
        synthesized_accessor_calls,
        private_property_accessor_calls,
    ];
    for facts in inherited {
        facts.inherit(source, target);
    }
    inherit_declaration_argument_boundaries(exprs, declaration_argument_boundaries, source, target);
}

/// A copied call's argument boundaries name the copy's own arguments, at the same positions.
fn inherit_declaration_argument_boundaries(
    exprs: &[IrExpr],
    boundaries: &mut crate::fx_hash::HashMap<ExprId, Box<[super::IrDeclarationArgumentBoundary]>>,
    source: ExprId,
    target: ExprId,
) {
    let Some(recorded) = boundaries.get(&source) else {
        return;
    };
    let supplied = |expression: ExprId| match &exprs[expression as usize] {
        IrExpr::Call { args, .. } => args.clone(),
        IrExpr::MethodCall { args, .. } => args.iter().copied().flatten().collect(),
        _ => Vec::new(),
    };
    let source_arguments = supplied(source);
    let target_arguments = supplied(target);
    let copied = recorded
        .iter()
        .map(|boundary| {
            let position = source_arguments
                .iter()
                .position(|argument| *argument == boundary.argument)
                .expect("recorded declaration argument belongs to cloned call");
            let mut boundary = *boundary;
            boundary.argument = target_arguments[position];
            boundary
        })
        .collect::<Box<[_]>>();
    boundaries.insert(target, copied);
}

#[cfg(test)]
mod tests {
    use super::super::{IrConst, IrExpr, IrFile};
    use super::copy_expression_facts;
    use crate::fir::SourceFileId;

    #[test]
    fn a_copy_inherits_the_facts_its_owning_modules_record() {
        let mut ir = IrFile::default();
        let source = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let target = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        ir.set_source_line(source, 4);
        ir.set_statement_line(source, 3);
        ir.mark_positionless(source);
        ir.mark_value_class_parameter_construction(source);
        ir.record_inline_copy_owner(source, 2, None, SourceFileId::from_raw(0));
        // The copy already sits in another expansion's frame.
        ir.record_inline_copy_owner(target, 3, None, SourceFileId::from_raw(1));

        copy_expression_facts(&mut ir, source, target);

        assert_eq!(ir.source_line(target), Some(4));
        assert_eq!(ir.statement_line(target), Some(3));
        assert!(ir.is_positionless(target));
        assert!(ir.has_value_class_parameter_construction(target));
        let frames = ir
            .inline_copy_provenance(target)
            .expect("the copy keeps its frames")
            .iter()
            .map(|frame| (frame.function, frame.source))
            .collect::<Vec<_>>();
        assert_eq!(
            frames,
            [
                (3, SourceFileId::from_raw(1)),
                (2, SourceFileId::from_raw(0))
            ]
        );
    }

    #[test]
    fn a_copy_does_not_inherit_the_physical_realization_of_its_source() {
        let mut ir = IrFile::default();
        let source = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let target = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        ir.jvm_member_targets.insert(source, 5);
        ir.initializer_blocks.insert(source);

        copy_expression_facts(&mut ir, source, target);

        assert_eq!(ir.jvm_member_targets.get(&target), None);
        assert!(!ir.initializer_blocks.contains(&target));
        assert_eq!(ir.jvm_member_targets.get(&source), Some(&5));
        assert!(ir.initializer_blocks.contains(&source));
    }
}
