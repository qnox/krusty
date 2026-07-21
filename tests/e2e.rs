//! Main integration-test target. Cargo builds each top-level tests/*.rs file as a separate
//! crate, so product e2e modules live behind one target to keep link time and target size bounded.
//! External corpus/reference-toolchain suites live in `conformance.rs`.

mod common;

#[path = "abstract_instantiation_check_e2e.rs"]
mod abstract_instantiation_check_e2e;
#[path = "abstract_member_check_e2e.rs"]
mod abstract_member_check_e2e;
#[path = "abstract_modifier_consistency_e2e.rs"]
mod abstract_modifier_consistency_e2e;
#[path = "annotation_instantiation_e2e.rs"]
mod annotation_instantiation_e2e;
#[path = "anon_fun_statement_e2e.rs"]
mod anon_fun_statement_e2e;
#[path = "anon_object_capture_e2e.rs"]
mod anon_object_capture_e2e;
#[path = "anon_object_capture_member_e2e.rs"]
mod anon_object_capture_member_e2e;
#[path = "anonymous_function_e2e.rs"]
mod anonymous_function_e2e;
#[path = "arity_error_coverage_e2e.rs"]
mod arity_error_coverage_e2e;
#[path = "assert_intrinsic_e2e.rs"]
mod assert_intrinsic_e2e;
#[path = "backend_rejection_coverage_e2e.rs"]
mod backend_rejection_coverage_e2e;
#[path = "backing_field_accessor_e2e.rs"]
mod backing_field_accessor_e2e;
#[path = "backtick_identifier_e2e.rs"]
mod backtick_identifier_e2e;
#[path = "bare_accessor_and_setter_bridge_e2e.rs"]
mod bare_accessor_and_setter_bridge_e2e;
#[path = "bound_expr_ref_e2e.rs"]
mod bound_expr_ref_e2e;
#[path = "bound_library_ref_e2e.rs"]
mod bound_library_ref_e2e;
#[path = "bounded_type_param_e2e.rs"]
mod bounded_type_param_e2e;
#[path = "boxed_array_construction_e2e.rs"]
mod boxed_array_construction_e2e;
#[path = "bracket_lambda_param_e2e.rs"]
mod bracket_lambda_param_e2e;
#[path = "break_continue_e2e.rs"]
mod break_continue_e2e;
#[path = "break_continue_expr_e2e.rs"]
mod break_continue_expr_e2e;
#[path = "break_continue_in_branch_e2e.rs"]
mod break_continue_in_branch_e2e;
#[path = "build1017_oo1_vcparam_result_takeif_e2e.rs"]
mod build1017_oo1_vcparam_result_takeif_e2e;
#[path = "build1018_pp1_suspend_block_firstornull_e2e.rs"]
mod build1018_pp1_suspend_block_firstornull_e2e;
#[path = "build688_cc1_bb1_e2e.rs"]
mod build688_cc1_bb1_e2e;
#[path = "build688_ff1_suspend_hof_e2e.rs"]
mod build688_ff1_suspend_hof_e2e;
#[path = "build702_aa1_suspend_nullable_elvis_e2e.rs"]
mod build702_aa1_suspend_nullable_elvis_e2e;
#[path = "build702_dd1_suspend_default_e2e.rs"]
mod build702_dd1_suspend_default_e2e;
#[path = "build702_fq_trailing_lambda_e2e.rs"]
mod build702_fq_trailing_lambda_e2e;
#[path = "build702_gg1_sealed_when_e2e.rs"]
mod build702_gg1_sealed_when_e2e;
#[path = "build722_aa1_suspend_nullable_elvis_e2e.rs"]
mod build722_aa1_suspend_nullable_elvis_e2e;
#[path = "build722_dd1_suspend_member_default_e2e.rs"]
mod build722_dd1_suspend_member_default_e2e;
#[path = "build722_hh1_inline_hof_both_branches_e2e.rs"]
mod build722_hh1_inline_hof_both_branches_e2e;
#[path = "build722_reified_class_literal_e2e.rs"]
mod build722_reified_class_literal_e2e;
#[path = "build775_aa1_suspend_iface_param_elvis_e2e.rs"]
mod build775_aa1_suspend_iface_param_elvis_e2e;
#[path = "build775_ee1_reified_vc_ext_e2e.rs"]
mod build775_ee1_reified_vc_ext_e2e;
#[path = "build775_ii1_suspend_for_loop_e2e.rs"]
mod build775_ii1_suspend_for_loop_e2e;
#[path = "build840_collection_property_element_e2e.rs"]
mod build840_collection_property_element_e2e;
#[path = "build840_jj1_param_soft_keyword_e2e.rs"]
mod build840_jj1_param_soft_keyword_e2e;
#[path = "build840_kk1_inline_hof_enclosing_member_e2e.rs"]
mod build840_kk1_inline_hof_enclosing_member_e2e;
#[path = "build840_mm1_safe_call_lambda_ext_e2e.rs"]
mod build840_mm1_safe_call_lambda_ext_e2e;
#[path = "build840_nn1_suspend_inline_withlock_e2e.rs"]
mod build840_nn1_suspend_inline_withlock_e2e;
#[path = "builtin_intrinsics_coverage_e2e.rs"]
mod builtin_intrinsics_coverage_e2e;
#[path = "bytecode_parity_e2e.rs"]
mod bytecode_parity_e2e;
#[path = "callable_ref_e2e.rs"]
mod callable_ref_e2e;
#[path = "callable_ref_equality_e2e.rs"]
mod callable_ref_equality_e2e;
#[path = "callable_ref_extension_e2e.rs"]
mod callable_ref_extension_e2e;
#[path = "catch_annotation_comma_e2e.rs"]
mod catch_annotation_comma_e2e;
#[path = "checker_operator_methods_e2e.rs"]
mod checker_operator_methods_e2e;
#[path = "class_body_e2e.rs"]
mod class_body_e2e;
#[path = "class_literal_e2e.rs"]
mod class_literal_e2e;
#[path = "class_metadata_roundtrip.rs"]
mod class_metadata_roundtrip;
#[path = "class_tparam_cast_e2e.rs"]
mod class_tparam_cast_e2e;
#[path = "classfile_e2e.rs"]
mod classfile_e2e;
#[path = "classpath_annotation_emit_e2e.rs"]
mod classpath_annotation_emit_e2e;
#[path = "classpath_collection_and_nested_named_e2e.rs"]
mod classpath_collection_and_nested_named_e2e;
#[path = "classpath_collection_param_member_e2e.rs"]
mod classpath_collection_param_member_e2e;
#[path = "classpath_companion.rs"]
mod classpath_companion;
#[path = "classpath_companion_invoke_e2e.rs"]
mod classpath_companion_invoke_e2e;
#[path = "classpath_data_copy_e2e.rs"]
mod classpath_data_copy_e2e;
#[path = "classpath_default_args_e2e.rs"]
mod classpath_default_args_e2e;
#[path = "classpath_enum_regex_vc_e2e.rs"]
mod classpath_enum_regex_vc_e2e;
#[path = "classpath_function_reference_e2e.rs"]
mod classpath_function_reference_e2e;
#[path = "classpath_is_smartcast_e2e.rs"]
mod classpath_is_smartcast_e2e;
#[path = "classpath_nested_ctor_reordered_named_valueclass_e2e.rs"]
mod classpath_nested_ctor_reordered_named_valueclass_e2e;
#[path = "classpath_object_member_import_e2e.rs"]
mod classpath_object_member_import_e2e;
#[path = "classpath_object_nested_e2e.rs"]
mod classpath_object_nested_e2e;
#[path = "classpath_object_value_e2e.rs"]
mod classpath_object_value_e2e;
#[path = "classpath_properties_query_e2e.rs"]
mod classpath_properties_query_e2e;
#[path = "classpath_protected_member_e2e.rs"]
mod classpath_protected_member_e2e;
#[path = "classpath_qualified_nested_named_ctor_e2e.rs"]
mod classpath_qualified_nested_named_ctor_e2e;
#[path = "classpath_receiver_lambda_e2e.rs"]
mod classpath_receiver_lambda_e2e;
#[path = "classpath_runblocking_e2e.rs"]
mod classpath_runblocking_e2e;
#[path = "classpath_subtype_ctor_arg_e2e.rs"]
mod classpath_subtype_ctor_arg_e2e;
#[path = "classpath_super_ctor_args_e2e.rs"]
mod classpath_super_ctor_args_e2e;
#[path = "classpath_synthetic_ctor_e2e.rs"]
mod classpath_synthetic_ctor_e2e;
#[path = "classpath_type_ref_e2e.rs"]
mod classpath_type_ref_e2e;
#[path = "classpath_typealias_e2e.rs"]
mod classpath_typealias_e2e;
#[path = "classpath_value_class_default_e2e.rs"]
mod classpath_value_class_default_e2e;
#[path = "classpath_value_class_member_e2e.rs"]
mod classpath_value_class_member_e2e;
#[path = "classpath_valueclass_param_ext_e2e.rs"]
mod classpath_valueclass_param_ext_e2e;
#[path = "classreader_e2e.rs"]
mod classreader_e2e;
#[path = "cli_dropin_e2e.rs"]
mod cli_dropin_e2e;
#[path = "closure_in_class_e2e.rs"]
mod closure_in_class_e2e;
#[path = "codegen_host_e2e.rs"]
mod codegen_host_e2e;
#[path = "collection_members_e2e.rs"]
mod collection_members_e2e;
#[path = "collection_special_member_stub_e2e.rs"]
mod collection_special_member_stub_e2e;
#[path = "companion_const_e2e.rs"]
mod companion_const_e2e;
#[path = "companion_e2e.rs"]
mod companion_e2e;
#[path = "companion_non_const_prop_e2e.rs"]
mod companion_non_const_prop_e2e;
#[path = "companion_supertype_e2e.rs"]
mod companion_supertype_e2e;
#[path = "compare_to_zero_branch_e2e.rs"]
mod compare_to_zero_branch_e2e;
#[path = "compound_index_assign_e2e.rs"]
mod compound_index_assign_e2e;
#[path = "compound_member_assign_lhs_caching_e2e.rs"]
mod compound_member_assign_lhs_caching_e2e;
#[path = "computed_prop_e2e.rs"]
mod computed_prop_e2e;
#[path = "computed_prop_generic_return_e2e.rs"]
mod computed_prop_generic_return_e2e;
#[path = "const_constantvalue_e2e.rs"]
mod const_constantvalue_e2e;
#[path = "const_read_inline_e2e.rs"]
mod const_read_inline_e2e;
#[path = "const_val_e2e.rs"]
mod const_val_e2e;
#[path = "construction_default_arg_e2e.rs"]
mod construction_default_arg_e2e;
#[path = "context_function_type_e2e.rs"]
mod context_function_type_e2e;
#[path = "context_local_fun_e2e.rs"]
mod context_local_fun_e2e;
#[path = "context_parameters_e2e.rs"]
mod context_parameters_e2e;
#[path = "contract_erasure_e2e.rs"]
mod contract_erasure_e2e;
#[path = "coroutine_intrinsics_e2e.rs"]
mod coroutine_intrinsics_e2e;
#[path = "cross_file_ctor_default_e2e.rs"]
mod cross_file_ctor_default_e2e;
#[path = "data_class_metadata_wiring_e2e.rs"]
mod data_class_metadata_wiring_e2e;
#[path = "data_class_param_check_e2e.rs"]
mod data_class_param_check_e2e;
#[path = "data_copy_e2e.rs"]
mod data_copy_e2e;
#[path = "dataclass_hash_and_sam_e2e.rs"]
mod dataclass_hash_and_sam_e2e;
#[path = "decl_body_on_next_line_e2e.rs"]
mod decl_body_on_next_line_e2e;
#[path = "deep_nested_type_e2e.rs"]
mod deep_nested_type_e2e;
#[path = "default_args_member_e2e.rs"]
mod default_args_member_e2e;
#[path = "default_args_synthetic_e2e.rs"]
mod default_args_synthetic_e2e;
#[path = "default_import_resolution_e2e.rs"]
mod default_import_resolution_e2e;
#[path = "deferred_val_init_e2e.rs"]
mod deferred_val_init_e2e;
#[path = "definitely_non_null_type_e2e.rs"]
mod definitely_non_null_type_e2e;
#[path = "delegate_by_lazy_e2e.rs"]
mod delegate_by_lazy_e2e;
#[path = "delegated_local_prop_e2e.rs"]
mod delegated_local_prop_e2e;
#[path = "delegated_member_prop_e2e.rs"]
mod delegated_member_prop_e2e;
#[path = "delegated_prop_e2e.rs"]
mod delegated_prop_e2e;
#[path = "dep_resolution.rs"]
mod dep_resolution;
#[path = "destructure_component_extension_e2e.rs"]
mod destructure_component_extension_e2e;
#[path = "destructure_e2e.rs"]
mod destructure_e2e;
#[path = "diagnostic_markers_e2e.rs"]
mod diagnostic_markers_e2e;
#[path = "diagnostics_match_kotlinc.rs"]
mod diagnostics_match_kotlinc;
#[path = "diverging_init_e2e.rs"]
mod diverging_init_e2e;
#[path = "do_while_e2e.rs"]
mod do_while_e2e;
#[path = "dotted_extension_receiver_e2e.rs"]
mod dotted_extension_receiver_e2e;
#[path = "duplicate_ctor_param_check_e2e.rs"]
mod duplicate_ctor_param_check_e2e;
#[path = "duplicate_enum_entry_check_e2e.rs"]
mod duplicate_enum_entry_check_e2e;
#[path = "duplicate_param_check_e2e.rs"]
mod duplicate_param_check_e2e;
#[path = "elvis_newline_continuation_e2e.rs"]
mod elvis_newline_continuation_e2e;
#[path = "elvis_nullability_join_e2e.rs"]
mod elvis_nullability_join_e2e;
#[path = "empty_loop_body_e2e.rs"]
mod empty_loop_body_e2e;
#[path = "enum_body_property_e2e.rs"]
mod enum_body_property_e2e;
#[path = "enum_class_signature_e2e.rs"]
mod enum_class_signature_e2e;
#[path = "enum_constant_annotation_e2e.rs"]
mod enum_constant_annotation_e2e;
#[path = "enum_constant_annotation_emit_e2e.rs"]
mod enum_constant_annotation_emit_e2e;
#[path = "enum_ctor_default_arg_e2e.rs"]
mod enum_ctor_default_arg_e2e;
#[path = "enum_entries_e2e.rs"]
mod enum_entries_e2e;
#[path = "enum_entry_named_arg_e2e.rs"]
mod enum_entry_named_arg_e2e;
#[path = "enum_entry_property_e2e.rs"]
mod enum_entry_property_e2e;
#[path = "enum_generic_interface_e2e.rs"]
mod enum_generic_interface_e2e;
#[path = "enum_implements_interface_e2e.rs"]
mod enum_implements_interface_e2e;
#[path = "enum_vararg_e2e.rs"]
mod enum_vararg_e2e;
#[path = "expected_type_propagation_e2e.rs"]
mod expected_type_propagation_e2e;
#[path = "expr_completeness_e2e.rs"]
mod expr_completeness_e2e;
#[path = "ext_on_subtype_receiver_e2e.rs"]
mod ext_on_subtype_receiver_e2e;
#[path = "extension_default_args_e2e.rs"]
mod extension_default_args_e2e;
#[path = "extension_fun_e2e.rs"]
mod extension_fun_e2e;
#[path = "extension_property_e2e.rs"]
mod extension_property_e2e;
#[path = "facade_emission_e2e.rs"]
mod facade_emission_e2e;
#[path = "feature_box_e2e.rs"]
mod feature_box_e2e;
#[path = "feature_coverage_a_e2e.rs"]
mod feature_coverage_a_e2e;
#[path = "feature_coverage_b_e2e.rs"]
mod feature_coverage_b_e2e;
#[path = "feature_coverage_c_e2e.rs"]
mod feature_coverage_c_e2e;
#[path = "feature_coverage_d_e2e.rs"]
mod feature_coverage_d_e2e;
#[path = "feature_coverage_e_e2e.rs"]
mod feature_coverage_e_e2e;
#[path = "feature_coverage_g_e2e.rs"]
mod feature_coverage_g_e2e;
#[path = "feature_coverage_h_e2e.rs"]
mod feature_coverage_h_e2e;
#[path = "feature_coverage_i_e2e.rs"]
mod feature_coverage_i_e2e;
#[path = "feature_coverage_j_e2e.rs"]
mod feature_coverage_j_e2e;
#[path = "feature_coverage_k_e2e.rs"]
mod feature_coverage_k_e2e;
#[path = "feature_coverage_l_e2e.rs"]
mod feature_coverage_l_e2e;
#[path = "feature_coverage_m_e2e.rs"]
mod feature_coverage_m_e2e;
#[path = "feature_coverage_n_e2e.rs"]
mod feature_coverage_n_e2e;
#[path = "feature_coverage_o_e2e.rs"]
mod feature_coverage_o_e2e;
#[path = "feature_coverage_p_e2e.rs"]
mod feature_coverage_p_e2e;
#[path = "feature_coverage_q_e2e.rs"]
mod feature_coverage_q_e2e;
#[path = "feature_coverage_r_e2e.rs"]
mod feature_coverage_r_e2e;
#[path = "feature_coverage_s_e2e.rs"]
mod feature_coverage_s_e2e;
#[path = "feature_coverage_t_e2e.rs"]
mod feature_coverage_t_e2e;
#[path = "feature_coverage_u_e2e.rs"]
mod feature_coverage_u_e2e;
#[path = "feature_coverage_v_e2e.rs"]
mod feature_coverage_v_e2e;
#[path = "feature_coverage_w_e2e.rs"]
mod feature_coverage_w_e2e;
#[path = "feature_coverage_x_e2e.rs"]
mod feature_coverage_x_e2e;
#[path = "finally_e2e.rs"]
mod finally_e2e;
#[path = "float_range_nan_e2e.rs"]
mod float_range_nan_e2e;
#[path = "for_iterable_elvis_e2e.rs"]
mod for_iterable_elvis_e2e;
#[path = "for_typed_loop_var_e2e.rs"]
mod for_typed_loop_var_e2e;
#[path = "fq_ctor_call_e2e.rs"]
mod fq_ctor_call_e2e;
#[path = "fq_static_call_e2e.rs"]
mod fq_static_call_e2e;
#[path = "fq_toplevel_call_e2e.rs"]
mod fq_toplevel_call_e2e;
#[path = "front_end_errors_e2e.rs"]
mod front_end_errors_e2e;
#[path = "front_end_errors_more_e2e.rs"]
mod front_end_errors_more_e2e;
#[path = "full_form_destructuring_e2e.rs"]
mod full_form_destructuring_e2e;
#[path = "function_type_is_e2e.rs"]
mod function_type_is_e2e;
#[path = "function_type_supertype_e2e.rs"]
mod function_type_supertype_e2e;
#[path = "function_typed_property_e2e.rs"]
mod function_typed_property_e2e;
#[path = "generic_base_member_type_e2e.rs"]
mod generic_base_member_type_e2e;
#[path = "generic_delegate_e2e.rs"]
mod generic_delegate_e2e;
#[path = "generic_fn_e2e.rs"]
mod generic_fn_e2e;
#[path = "generic_hof_method_check.rs"]
mod generic_hof_method_check;
#[path = "generic_inferred_return_e2e.rs"]
mod generic_inferred_return_e2e;
#[path = "generic_return_inference_e2e.rs"]
mod generic_return_inference_e2e;
#[path = "generic_signature_e2e.rs"]
mod generic_signature_e2e;
#[path = "generic_suspend_member_return_e2e.rs"]
mod generic_suspend_member_return_e2e;
#[path = "implicit_this_member_hof_lambda_e2e.rs"]
mod implicit_this_member_hof_lambda_e2e;
#[path = "implicit_this_method_ref_e2e.rs"]
mod implicit_this_method_ref_e2e;
#[path = "import_scope_conformance_e2e.rs"]
mod import_scope_conformance_e2e;
#[path = "indy_infra_e2e.rs"]
mod indy_infra_e2e;
#[path = "inferred_computed_prop_e2e.rs"]
mod inferred_computed_prop_e2e;
#[path = "inferred_property_type_args_e2e.rs"]
mod inferred_property_type_args_e2e;
#[path = "inheritance_e2e.rs"]
mod inheritance_e2e;
#[path = "inline_deep_coverage_e2e.rs"]
mod inline_deep_coverage_e2e;
#[path = "inline_e2e.rs"]
mod inline_e2e;
#[path = "inline_end_branch_e2e.rs"]
mod inline_end_branch_e2e;
#[path = "inline_lambda_value_return_e2e.rs"]
mod inline_lambda_value_return_e2e;
#[path = "inline_splice_e2e.rs"]
mod inline_splice_e2e;
#[path = "inline_splice_ldc_wide_e2e.rs"]
mod inline_splice_ldc_wide_e2e;
#[path = "inline_vc_suspend_coverage_e2e.rs"]
mod inline_vc_suspend_coverage_e2e;
#[path = "inner_class_construction_e2e.rs"]
mod inner_class_construction_e2e;
#[path = "inner_class_outer_tparam_e2e.rs"]
mod inner_class_outer_tparam_e2e;
#[path = "interface_companion_e2e.rs"]
mod interface_companion_e2e;
#[path = "interface_default_args_e2e.rs"]
mod interface_default_args_e2e;
#[path = "interface_default_method_e2e.rs"]
mod interface_default_method_e2e;
#[path = "interface_delegation_e2e.rs"]
mod interface_delegation_e2e;
#[path = "interface_delegation_expr_e2e.rs"]
mod interface_delegation_expr_e2e;
#[path = "interface_supertype_members_e2e.rs"]
mod interface_supertype_members_e2e;
#[path = "invoke_operator_extension_e2e.rs"]
mod invoke_operator_extension_e2e;
#[path = "ir_edge_coverage_e2e.rs"]
mod ir_edge_coverage_e2e;
#[path = "ir_lower_bail_coverage_e2e.rs"]
mod ir_lower_bail_coverage_e2e;
#[path = "ir_lower_deep_coverage_e2e.rs"]
mod ir_lower_deep_coverage_e2e;
#[path = "is_nullable_primitive_e2e.rs"]
mod is_nullable_primitive_e2e;
#[path = "is_primitive_smartcast_e2e.rs"]
mod is_primitive_smartcast_e2e;
#[path = "java_instance_e2e.rs"]
mod java_instance_e2e;
#[path = "jimage_compressed_e2e.rs"]
mod jimage_compressed_e2e;
#[path = "js_backend_coverage_e2e.rs"]
mod js_backend_coverage_e2e;
#[path = "js_backend_e2e.rs"]
mod js_backend_e2e;
#[path = "krusty_dep_dir_e2e.rs"]
mod krusty_dep_dir_e2e;
#[path = "ksp_provision_e2e.rs"]
mod ksp_provision_e2e;
#[path = "labeled_expression_e2e.rs"]
mod labeled_expression_e2e;
#[path = "labeled_this_e2e.rs"]
mod labeled_this_e2e;
#[path = "lambda_e2e.rs"]
mod lambda_e2e;
#[path = "lambda_vs_block_fun_type_e2e.rs"]
mod lambda_vs_block_fun_type_e2e;
#[path = "lateinit_local_e2e.rs"]
mod lateinit_local_e2e;
#[path = "list_fold_e2e.rs"]
mod list_fold_e2e;
#[path = "literal_escapes_coverage_e2e.rs"]
mod literal_escapes_coverage_e2e;
#[path = "local_capture_coverage_e2e.rs"]
mod local_capture_coverage_e2e;
#[path = "local_class_e2e.rs"]
mod local_class_e2e;
#[path = "local_class_scoping_e2e.rs"]
mod local_class_scoping_e2e;
#[path = "local_fun_default_args_e2e.rs"]
mod local_fun_default_args_e2e;
#[path = "local_fun_ref_e2e.rs"]
mod local_fun_ref_e2e;
#[path = "mangled_member_concrete_class_e2e.rs"]
mod mangled_member_concrete_class_e2e;
#[path = "mangled_member_nested_param_e2e.rs"]
mod mangled_member_nested_param_e2e;
#[path = "mangled_member_null_arg_e2e.rs"]
mod mangled_member_null_arg_e2e;
#[path = "map_entry_destructure_e2e.rs"]
mod map_entry_destructure_e2e;
#[path = "map_get_nullable_elvis_e2e.rs"]
mod map_get_nullable_elvis_e2e;
#[path = "member_array_ctor_inference_e2e.rs"]
mod member_array_ctor_inference_e2e;
#[path = "member_ctrl_inference_e2e.rs"]
mod member_ctrl_inference_e2e;
#[path = "member_default_implicit_receiver_e2e.rs"]
mod member_default_implicit_receiver_e2e;
#[path = "member_infix_inference_e2e.rs"]
mod member_infix_inference_e2e;
#[path = "member_read_on_nullable_call_result_e2e.rs"]
mod member_read_on_nullable_call_result_e2e;
#[path = "metadata_kept_params.rs"]
mod metadata_kept_params;
#[path = "metadata_reader_e2e.rs"]
mod metadata_reader_e2e;
#[path = "metadata_return_types.rs"]
mod metadata_return_types;
#[path = "missing_return_check_e2e.rs"]
mod missing_return_check_e2e;
#[path = "multi_index_operator_e2e.rs"]
mod multi_index_operator_e2e;
#[path = "multiline_catch_e2e.rs"]
mod multiline_catch_e2e;
#[path = "mutable_property_ref_e2e.rs"]
mod mutable_property_ref_e2e;
#[path = "name_based_destructuring_e2e.rs"]
mod name_based_destructuring_e2e;
#[path = "named_arg_member_e2e.rs"]
mod named_arg_member_e2e;
#[path = "named_arg_source_order_e2e.rs"]
mod named_arg_source_order_e2e;
#[path = "named_args_classpath_e2e.rs"]
mod named_args_classpath_e2e;
#[path = "named_ctor_args_e2e.rs"]
mod named_ctor_args_e2e;
#[path = "named_super_arg_e2e.rs"]
mod named_super_arg_e2e;
#[path = "narrowed_this_member_call_e2e.rs"]
mod narrowed_this_member_call_e2e;
#[path = "nested_class_supertype_e2e.rs"]
mod nested_class_supertype_e2e;
#[path = "nested_class_unqualified_e2e.rs"]
mod nested_class_unqualified_e2e;
#[path = "nested_ctor_named_args_e2e.rs"]
mod nested_ctor_named_args_e2e;
#[path = "nested_decls_in_object_e2e.rs"]
mod nested_decls_in_object_e2e;
#[path = "nested_enum_access_e2e.rs"]
mod nested_enum_access_e2e;
#[path = "nested_hof_capture_e2e.rs"]
mod nested_hof_capture_e2e;
#[path = "nested_lambda_capture_e2e.rs"]
mod nested_lambda_capture_e2e;
#[path = "nested_string_template_e2e.rs"]
mod nested_string_template_e2e;
#[path = "nested_try_finally_e2e.rs"]
mod nested_try_finally_e2e;
#[path = "nested_type_scope_e2e.rs"]
mod nested_type_scope_e2e;
#[path = "nested_type_shadowing_e2e.rs"]
mod nested_type_shadowing_e2e;
#[path = "newline_method_chain_e2e.rs"]
mod newline_method_chain_e2e;
#[path = "not_null_assert_e2e.rs"]
mod not_null_assert_e2e;
#[path = "nothing_call_branch_e2e.rs"]
mod nothing_call_branch_e2e;
#[path = "nothing_nullable_e2e.rs"]
mod nothing_nullable_e2e;
#[path = "nullable_cast_e2e.rs"]
mod nullable_cast_e2e;
#[path = "nullable_function_type_e2e.rs"]
mod nullable_function_type_e2e;
#[path = "nullable_primitive_box_e2e.rs"]
mod nullable_primitive_box_e2e;
#[path = "nullable_ref_arg_to_erased_param_e2e.rs"]
mod nullable_ref_arg_to_erased_param_e2e;
#[path = "nullable_unit_e2e.rs"]
mod nullable_unit_e2e;
#[path = "nullable_vc_default_stub_e2e.rs"]
mod nullable_vc_default_stub_e2e;
#[path = "number_and_ctor_coverage_e2e.rs"]
mod number_and_ctor_coverage_e2e;
#[path = "number_assignability_e2e.rs"]
mod number_assignability_e2e;
#[path = "numeric_eq_conform_e2e.rs"]
mod numeric_eq_conform_e2e;
#[path = "numeric_ops_coverage_e2e.rs"]
mod numeric_ops_coverage_e2e;
#[path = "object_const_val_e2e.rs"]
mod object_const_val_e2e;
#[path = "object_default_ctor_arg_e2e.rs"]
mod object_default_ctor_arg_e2e;
#[path = "object_extends_class_e2e.rs"]
mod object_extends_class_e2e;
#[path = "object_member_import_e2e.rs"]
mod object_member_import_e2e;
#[path = "object_member_ref_import_e2e.rs"]
mod object_member_ref_import_e2e;
#[path = "object_method_ref_e2e.rs"]
mod object_method_ref_e2e;
#[path = "object_value_inference_e2e.rs"]
mod object_value_inference_e2e;
#[path = "operator_inc_dec_e2e.rs"]
mod operator_inc_dec_e2e;
#[path = "operator_index_e2e.rs"]
mod operator_index_e2e;
#[path = "overloaded_extension_e2e.rs"]
mod overloaded_extension_e2e;
#[path = "overloaded_inferred_return_e2e.rs"]
mod overloaded_inferred_return_e2e;
#[path = "pair_triple_e2e.rs"]
mod pair_triple_e2e;
#[path = "paren_condition_newline_e2e.rs"]
mod paren_condition_newline_e2e;
#[path = "parser_errors_coverage_e2e.rs"]
mod parser_errors_coverage_e2e;
#[path = "plugins_e2e.rs"]
mod plugins_e2e;
#[path = "primitive_bound_generic_e2e.rs"]
mod primitive_bound_generic_e2e;
#[path = "primitive_box_cast_e2e.rs"]
mod primitive_box_cast_e2e;
#[path = "primitive_operator_extension_e2e.rs"]
mod primitive_operator_extension_e2e;
#[path = "primitive_spread_e2e.rs"]
mod primitive_spread_e2e;
#[path = "private_set_e2e.rs"]
mod private_set_e2e;
#[path = "property_accessor_e2e.rs"]
mod property_accessor_e2e;
#[path = "property_conversion_inference_e2e.rs"]
mod property_conversion_inference_e2e;
#[path = "property_infer_member_e2e.rs"]
mod property_infer_member_e2e;
#[path = "qq1_safecall_diverging_scope_block_e2e.rs"]
mod qq1_safecall_diverging_scope_block_e2e;
#[path = "range_property_e2e.rs"]
mod range_property_e2e;
#[path = "range_step_e2e.rs"]
mod range_step_e2e;
#[path = "raw_string_interpolation_e2e.rs"]
mod raw_string_interpolation_e2e;
#[path = "receiver_lambda_e2e.rs"]
mod receiver_lambda_e2e;
#[path = "reference_adaptation_e2e.rs"]
mod reference_adaptation_e2e;
#[path = "reference_in_range_e2e.rs"]
mod reference_in_range_e2e;
#[path = "reified_inline_check_e2e.rs"]
mod reified_inline_check_e2e;
#[path = "require_check_smartcast_e2e.rs"]
mod require_check_smartcast_e2e;
#[path = "resolve_parse_deep_coverage_e2e.rs"]
mod resolve_parse_deep_coverage_e2e;
#[path = "resolve_parser_diag_coverage_e2e.rs"]
mod resolve_parser_diag_coverage_e2e;
#[path = "resolver_errors_coverage_e2e.rs"]
mod resolver_errors_coverage_e2e;
#[path = "resolver_regression_e2e.rs"]
mod resolver_regression_e2e;
#[path = "result_e2e.rs"]
mod result_e2e;
#[path = "run_noreceiver_e2e.rs"]
mod run_noreceiver_e2e;
#[path = "safe_call_e2e.rs"]
mod safe_call_e2e;
#[path = "safe_call_generic_field_e2e.rs"]
mod safe_call_generic_field_e2e;
#[path = "safe_call_let_destructure_e2e.rs"]
mod safe_call_let_destructure_e2e;
#[path = "safe_call_member_on_generic_result_e2e.rs"]
mod safe_call_member_on_generic_result_e2e;
#[path = "safe_call_prim_intrinsic_e2e.rs"]
mod safe_call_prim_intrinsic_e2e;
#[path = "safe_cast_elvis_e2e.rs"]
mod safe_cast_elvis_e2e;
#[path = "sam_classpath_e2e.rs"]
mod sam_classpath_e2e;
#[path = "sam_conversion_e2e.rs"]
mod sam_conversion_e2e;
#[path = "same_package_classpath_e2e.rs"]
mod same_package_classpath_e2e;
#[path = "samefile_nested_object_value_e2e.rs"]
mod samefile_nested_object_value_e2e;
#[path = "scope_function_value_arg_e2e.rs"]
mod scope_function_value_arg_e2e;
#[path = "sealed_interface_nested_e2e.rs"]
mod sealed_interface_nested_e2e;
#[path = "sealed_object_value_match_e2e.rs"]
mod sealed_object_value_match_e2e;
#[path = "secondary_ctor_noprimary_e2e.rs"]
mod secondary_ctor_noprimary_e2e;
#[path = "secondary_ctor_this_sibling_e2e.rs"]
mod secondary_ctor_this_sibling_e2e;
#[path = "serialization_coverage_e2e.rs"]
mod serialization_coverage_e2e;
#[path = "serialization_krusty_only_e2e.rs"]
mod serialization_krusty_only_e2e;
#[path = "serialization_roundtrip_e2e.rs"]
mod serialization_roundtrip_e2e;
#[path = "session_subsystems_e2e.rs"]
mod session_subsystems_e2e;
#[path = "shadowed_method_tparam_e2e.rs"]
mod shadowed_method_tparam_e2e;
#[path = "short_circuit_e2e.rs"]
mod short_circuit_e2e;
#[path = "smartcast_and_e2e.rs"]
mod smartcast_and_e2e;
#[path = "spread_in_annotation_e2e.rs"]
mod spread_in_annotation_e2e;
#[path = "spread_operator_e2e.rs"]
mod spread_operator_e2e;
#[path = "static_member_import_e2e.rs"]
mod static_member_import_e2e;
#[path = "stdlib_call_resolution_e2e.rs"]
mod stdlib_call_resolution_e2e;
#[path = "string_concat_append_overload_e2e.rs"]
mod string_concat_append_overload_e2e;
#[path = "subtype_receiver_extension_call_e2e.rs"]
mod subtype_receiver_extension_call_e2e;
#[path = "super_default_args_e2e.rs"]
mod super_default_args_e2e;
#[path = "super_interface_default_e2e.rs"]
mod super_interface_default_e2e;
#[path = "super_to_base_secondary_ctor_e2e.rs"]
mod super_to_base_secondary_ctor_e2e;
#[path = "supertype_scan_arrow_e2e.rs"]
mod supertype_scan_arrow_e2e;
#[path = "suspend_class_implements_interface_e2e.rs"]
mod suspend_class_implements_interface_e2e;
#[path = "suspend_collection_hof_e2e.rs"]
mod suspend_collection_hof_e2e;
#[path = "suspend_collection_hof_suspend_lambda_e2e.rs"]
mod suspend_collection_hof_suspend_lambda_e2e;
#[path = "suspend_continuation_owner_e2e.rs"]
mod suspend_continuation_owner_e2e;
#[path = "suspend_default_param_e2e.rs"]
mod suspend_default_param_e2e;
#[path = "suspend_e2e.rs"]
mod suspend_e2e;
#[path = "suspend_inline_hof_suspending_lambda_reject_e2e.rs"]
mod suspend_inline_hof_suspending_lambda_reject_e2e;
#[path = "suspend_inline_statementless_block_e2e.rs"]
mod suspend_inline_statementless_block_e2e;
#[path = "suspend_loop_compound_assign_e2e.rs"]
mod suspend_loop_compound_assign_e2e;
#[path = "suspend_loop_continue_break_e2e.rs"]
mod suspend_loop_continue_break_e2e;
#[path = "suspend_member_after_call_e2e.rs"]
mod suspend_member_after_call_e2e;
#[path = "suspend_return_type_recovery_e2e.rs"]
mod suspend_return_type_recovery_e2e;
#[path = "suspend_spill_slot_reuse_e2e.rs"]
mod suspend_spill_slot_reuse_e2e;
#[path = "suspend_try_finally_body_e2e.rs"]
mod suspend_try_finally_body_e2e;
#[path = "suspend_try_finally_e2e.rs"]
mod suspend_try_finally_e2e;
#[path = "suspend_value_class_mangle_e2e.rs"]
mod suspend_value_class_mangle_e2e;
#[path = "suspend_withlock_nonlocal_return_e2e.rs"]
mod suspend_withlock_nonlocal_return_e2e;
#[path = "synthetic_accessor_e2e.rs"]
mod synthetic_accessor_e2e;
#[path = "tailrec_e2e.rs"]
mod tailrec_e2e;
#[path = "this_callable_ref_e2e.rs"]
mod this_callable_ref_e2e;
#[path = "this_smartcast_e2e.rs"]
mod this_smartcast_e2e;
#[path = "throw_e2e.rs"]
mod throw_e2e;
#[path = "top_level_custom_accessor_e2e.rs"]
mod top_level_custom_accessor_e2e;
#[path = "top_level_generic_property_inference_e2e.rs"]
mod top_level_generic_property_inference_e2e;
#[path = "top_level_property_e2e.rs"]
mod top_level_property_e2e;
#[path = "toplevel_array_inference_e2e.rs"]
mod toplevel_array_inference_e2e;
#[path = "toplevel_default_multimask_e2e.rs"]
mod toplevel_default_multimask_e2e;
#[path = "toplevel_prop_with_companion_const_e2e.rs"]
mod toplevel_prop_with_companion_const_e2e;
#[path = "toplevel_property_inference_e2e.rs"]
mod toplevel_property_inference_e2e;
#[path = "toplevel_property_ref_e2e.rs"]
mod toplevel_property_ref_e2e;
#[path = "trailing_lambda_default_e2e.rs"]
mod trailing_lambda_default_e2e;
#[path = "trailing_lambda_named_args_e2e.rs"]
mod trailing_lambda_named_args_e2e;
#[path = "transitive_type_bound_e2e.rs"]
mod transitive_type_bound_e2e;
#[path = "trim_indent_e2e.rs"]
mod trim_indent_e2e;
#[path = "try_catch_e2e.rs"]
mod try_catch_e2e;
#[path = "try_catch_expr_generic_merge_e2e.rs"]
mod try_catch_expr_generic_merge_e2e;
#[path = "type_param_vararg_check_e2e.rs"]
mod type_param_vararg_check_e2e;
#[path = "typeparam_cast_e2e.rs"]
mod typeparam_cast_e2e;
#[path = "unit_as_any_e2e.rs"]
mod unit_as_any_e2e;
#[path = "unit_cast_e2e.rs"]
mod unit_cast_e2e;
#[path = "unit_value_e2e.rs"]
mod unit_value_e2e;
#[path = "unsigned_array_e2e.rs"]
mod unsigned_array_e2e;
#[path = "unsigned_ext_e2e.rs"]
mod unsigned_ext_e2e;
#[path = "unsigned_toplevel_e2e.rs"]
mod unsigned_toplevel_e2e;
#[path = "use_site_variance_e2e.rs"]
mod use_site_variance_e2e;
#[path = "val_backing_field_getter_e2e.rs"]
mod val_backing_field_getter_e2e;
#[path = "value_class_classpath_ctor_e2e.rs"]
mod value_class_classpath_ctor_e2e;
#[path = "value_class_e2e.rs"]
mod value_class_e2e;
#[path = "value_class_init_validate_e2e.rs"]
mod value_class_init_validate_e2e;
#[path = "value_class_map_key_e2e.rs"]
mod value_class_map_key_e2e;
#[path = "value_class_nullable_widen_return_e2e.rs"]
mod value_class_nullable_widen_return_e2e;
#[path = "value_class_param_generic_return_e2e.rs"]
mod value_class_param_generic_return_e2e;
#[path = "value_class_template_lambda_e2e.rs"]
mod value_class_template_lambda_e2e;
#[path = "var_extension_property_e2e.rs"]
mod var_extension_property_e2e;
#[path = "var_smartcast_after_assign_e2e.rs"]
mod var_smartcast_after_assign_e2e;
#[path = "vararg_e2e.rs"]
mod vararg_e2e;
#[path = "when_branch_if_no_else_e2e.rs"]
mod when_branch_if_no_else_e2e;
#[path = "when_lambda_branch_e2e.rs"]
mod when_lambda_branch_e2e;
#[path = "when_multiple_else_check_e2e.rs"]
mod when_multiple_else_check_e2e;
#[path = "when_nothing_arm_e2e.rs"]
mod when_nothing_arm_e2e;
#[path = "when_throwing_branch_e2e.rs"]
mod when_throwing_branch_e2e;
#[path = "wrapped_param_type_e2e.rs"]
mod wrapped_param_type_e2e;
