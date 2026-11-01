//! Inline-function integration coverage: splicing, regenerated objects, and inlined bytes.

use super::common;

#[path = "../apply_receiver_single_store_e2e.rs"]
mod apply_receiver_single_store_e2e;
#[path = "../inline_arguments_in_place_e2e.rs"]
mod inline_arguments_in_place_e2e;
#[path = "../inline_body_class_source_map_e2e.rs"]
mod inline_body_class_source_map_e2e;
#[path = "../inline_call_condition_line_e2e.rs"]
mod inline_call_condition_line_e2e;
#[path = "../inline_call_operand_spill_e2e.rs"]
mod inline_call_operand_spill_e2e;
#[path = "../inline_capture_default_e2e.rs"]
mod inline_capture_default_e2e;
#[path = "../inline_capture_splice_e2e.rs"]
mod inline_capture_splice_e2e;
#[path = "../inline_constructor_reference_e2e.rs"]
mod inline_constructor_reference_e2e;
#[path = "../inline_crossinline_object_e2e.rs"]
mod inline_crossinline_object_e2e;
#[path = "../inline_deep_coverage_e2e.rs"]
mod inline_deep_coverage_e2e;
#[path = "../inline_default_bytes_e2e.rs"]
mod inline_default_bytes_e2e;
#[path = "../inline_default_stub_e2e.rs"]
mod inline_default_stub_e2e;
#[path = "../inline_early_return_cleanup_e2e.rs"]
mod inline_early_return_cleanup_e2e;
#[path = "../inline_end_branch_e2e.rs"]
mod inline_end_branch_e2e;
#[path = "../inline_erased_bound_receiver_e2e.rs"]
mod inline_erased_bound_receiver_e2e;
#[path = "../inline_erased_capture_e2e.rs"]
mod inline_erased_capture_e2e;
#[path = "../inline_erased_parameter_e2e.rs"]
mod inline_erased_parameter_e2e;
#[path = "../inline_finally_marker_e2e.rs"]
mod inline_finally_marker_e2e;
#[path = "../inline_function_lambda_class_e2e.rs"]
mod inline_function_lambda_class_e2e;
#[path = "../inline_lambda_as_value_e2e.rs"]
mod inline_lambda_as_value_e2e;
#[path = "../inline_lambda_caller_loop_jump_e2e.rs"]
mod inline_lambda_caller_loop_jump_e2e;
#[path = "../inline_lambda_captured_literal_e2e.rs"]
mod inline_lambda_captured_literal_e2e;
#[path = "../inline_lambda_companion_initializer_e2e.rs"]
mod inline_lambda_companion_initializer_e2e;
#[path = "../inline_lambda_expansion_e2e.rs"]
mod inline_lambda_expansion_e2e;
#[path = "../inline_lambda_initializer_values_e2e.rs"]
mod inline_lambda_initializer_values_e2e;
#[path = "../inline_lambda_local_delegate_e2e.rs"]
mod inline_lambda_local_delegate_e2e;
#[path = "../inline_lambda_local_table_order_e2e.rs"]
mod inline_lambda_local_table_order_e2e;
#[path = "../inline_lambda_nested_frame_return_e2e.rs"]
mod inline_lambda_nested_frame_return_e2e;
#[path = "../inline_lambda_nested_return_e2e.rs"]
mod inline_lambda_nested_return_e2e;
#[path = "../inline_lambda_operand_stack_e2e.rs"]
mod inline_lambda_operand_stack_e2e;
#[path = "../inline_lambda_placed_capture_suspension_e2e.rs"]
mod inline_lambda_placed_capture_suspension_e2e;
#[path = "../inline_lambda_result_boxing_e2e.rs"]
mod inline_lambda_result_boxing_e2e;
#[path = "../inline_lambda_resumed_value_class_e2e.rs"]
mod inline_lambda_resumed_value_class_e2e;
#[path = "../inline_lambda_return_scope_e2e.rs"]
mod inline_lambda_return_scope_e2e;
#[path = "../inline_lambda_sibling_value_class_e2e.rs"]
mod inline_lambda_sibling_value_class_e2e;
#[path = "../inline_lambda_suspend_lambda_e2e.rs"]
mod inline_lambda_suspend_lambda_e2e;
#[path = "../inline_lambda_suspend_member_e2e.rs"]
mod inline_lambda_suspend_member_e2e;
#[path = "../inline_lambda_unintercepted_probe_e2e.rs"]
mod inline_lambda_unintercepted_probe_e2e;
#[path = "../inline_lambda_value_argument_e2e.rs"]
mod inline_lambda_value_argument_e2e;
#[path = "../inline_lambda_value_class_result_e2e.rs"]
mod inline_lambda_value_class_result_e2e;
#[path = "../inline_lambda_value_return_e2e.rs"]
mod inline_lambda_value_return_e2e;
#[path = "../inline_non_local_jump_finally_e2e.rs"]
mod inline_non_local_jump_finally_e2e;
#[path = "../inline_non_local_return_finally_e2e.rs"]
mod inline_non_local_return_finally_e2e;
#[path = "../inline_object_regeneration_e2e.rs"]
mod inline_object_regeneration_e2e;
#[path = "../inline_only_extension_property_e2e.rs"]
mod inline_only_extension_property_e2e;
#[path = "../inline_operand_copies_e2e.rs"]
mod inline_operand_copies_e2e;
#[path = "../inline_preparation_sibling_annotation_e2e.rs"]
mod inline_preparation_sibling_annotation_e2e;
#[path = "../inline_reified_enum_operations_e2e.rs"]
mod inline_reified_enum_operations_e2e;
#[path = "../inline_reified_property_e2e.rs"]
mod inline_reified_property_e2e;
#[path = "../inline_repeat_local_e2e.rs"]
mod inline_repeat_local_e2e;
#[path = "../inline_return_bound_context_e2e.rs"]
mod inline_return_bound_context_e2e;
#[path = "../inline_sam_wrapper_copy_e2e.rs"]
mod inline_sam_wrapper_copy_e2e;
#[path = "../inline_same_module_bytes_e2e.rs"]
mod inline_same_module_bytes_e2e;
#[path = "../inline_same_module_internal_bodies_e2e.rs"]
mod inline_same_module_internal_bodies_e2e;
#[path = "../inline_same_module_lambda_bodies_e2e.rs"]
mod inline_same_module_lambda_bodies_e2e;
#[path = "../inline_same_module_members_e2e.rs"]
mod inline_same_module_members_e2e;
#[path = "../inline_same_module_non_local_jumps_e2e.rs"]
mod inline_same_module_non_local_jumps_e2e;
#[path = "../inline_same_module_suspend_callers_e2e.rs"]
mod inline_same_module_suspend_callers_e2e;
#[path = "../inline_scope_function_bytes_e2e.rs"]
mod inline_scope_function_bytes_e2e;
#[path = "../inline_splice_e2e.rs"]
mod inline_splice_e2e;
#[path = "../inline_splice_ldc_wide_e2e.rs"]
mod inline_splice_ldc_wide_e2e;
#[path = "../inline_tail_expansion_shape_e2e.rs"]
mod inline_tail_expansion_shape_e2e;
#[path = "../inline_try_close_line_e2e.rs"]
mod inline_try_close_line_e2e;
#[path = "../inline_use_close_e2e.rs"]
mod inline_use_close_e2e;
#[path = "../inline_use_lambda_e2e.rs"]
mod inline_use_lambda_e2e;
#[path = "../inline_value_class_lambda_e2e.rs"]
mod inline_value_class_lambda_e2e;
#[path = "../inline_vc_suspend_coverage_e2e.rs"]
mod inline_vc_suspend_coverage_e2e;
