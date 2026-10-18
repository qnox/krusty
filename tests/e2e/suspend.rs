//! Suspend function, coroutine, and continuation integration coverage.

use super::common;

#[path = "../suspend_anonymous_object_name_e2e.rs"]
mod suspend_anonymous_object_name_e2e;
#[path = "../suspend_arg_order_e2e.rs"]
mod suspend_arg_order_e2e;
#[path = "../suspend_argument_operands_e2e.rs"]
mod suspend_argument_operands_e2e;
#[path = "../suspend_bytecode_transformer_e2e.rs"]
mod suspend_bytecode_transformer_e2e;
#[path = "../suspend_callable_cast_e2e.rs"]
mod suspend_callable_cast_e2e;
#[path = "../suspend_class_implements_interface_e2e.rs"]
mod suspend_class_implements_interface_e2e;
#[path = "../suspend_collection_hof_e2e.rs"]
mod suspend_collection_hof_e2e;
#[path = "../suspend_collection_hof_suspend_lambda_e2e.rs"]
mod suspend_collection_hof_suspend_lambda_e2e;
#[path = "../suspend_continuation_owner_e2e.rs"]
mod suspend_continuation_owner_e2e;
#[path = "../suspend_continuation_signature_e2e.rs"]
mod suspend_continuation_signature_e2e;
#[path = "../suspend_conversion_e2e.rs"]
mod suspend_conversion_e2e;
#[path = "../suspend_coroutine_inline_e2e.rs"]
mod suspend_coroutine_inline_e2e;
#[path = "../suspend_debug_metadata_e2e.rs"]
mod suspend_debug_metadata_e2e;
#[path = "../suspend_default_call_chained_receiver_e2e.rs"]
mod suspend_default_call_chained_receiver_e2e;
#[path = "../suspend_default_param_e2e.rs"]
mod suspend_default_param_e2e;
#[path = "../suspend_e2e.rs"]
mod suspend_e2e;
#[path = "../suspend_fun_interface_e2e.rs"]
mod suspend_fun_interface_e2e;
#[path = "../suspend_function_boxing_e2e.rs"]
mod suspend_function_boxing_e2e;
#[path = "../suspend_in_spliced_inline_e2e.rs"]
mod suspend_in_spliced_inline_e2e;
#[path = "../suspend_in_try_body_hoist_e2e.rs"]
mod suspend_in_try_body_hoist_e2e;
#[path = "../suspend_inline_collection_spill_e2e.rs"]
mod suspend_inline_collection_spill_e2e;
#[path = "../suspend_inline_hof_suspending_lambda_reject_e2e.rs"]
mod suspend_inline_hof_suspending_lambda_reject_e2e;
#[path = "../suspend_inline_splice_names_e2e.rs"]
mod suspend_inline_splice_names_e2e;
#[path = "../suspend_inline_splice_nonlocal_return_e2e.rs"]
mod suspend_inline_splice_nonlocal_return_e2e;
#[path = "../suspend_inline_stateless_finally_e2e.rs"]
mod suspend_inline_stateless_finally_e2e;
#[path = "../suspend_inline_statementless_block_e2e.rs"]
mod suspend_inline_statementless_block_e2e;
#[path = "../suspend_interface_delegation_e2e.rs"]
mod suspend_interface_delegation_e2e;
#[path = "../suspend_interface_impl_e2e.rs"]
mod suspend_interface_impl_e2e;
#[path = "../suspend_invoke_reference_e2e.rs"]
mod suspend_invoke_reference_e2e;
#[path = "../suspend_lambda_class_e2e.rs"]
mod suspend_lambda_class_e2e;
#[path = "../suspend_lambda_local_function_e2e.rs"]
mod suspend_lambda_local_function_e2e;
#[path = "../suspend_lambda_metadata_e2e.rs"]
mod suspend_lambda_metadata_e2e;
#[path = "../suspend_lambda_nested_declarations_e2e.rs"]
mod suspend_lambda_nested_declarations_e2e;
#[path = "../suspend_lambda_private_property_e2e.rs"]
mod suspend_lambda_private_property_e2e;
#[path = "../suspend_lambda_shared_capture_e2e.rs"]
mod suspend_lambda_shared_capture_e2e;
#[path = "../suspend_lambda_sites_e2e.rs"]
mod suspend_lambda_sites_e2e;
#[path = "../suspend_lambda_unit_tail_e2e.rs"]
mod suspend_lambda_unit_tail_e2e;
#[path = "../suspend_loop_compound_assign_e2e.rs"]
mod suspend_loop_compound_assign_e2e;
#[path = "../suspend_loop_continue_break_e2e.rs"]
mod suspend_loop_continue_break_e2e;
#[path = "../suspend_member_after_call_e2e.rs"]
mod suspend_member_after_call_e2e;
#[path = "../suspend_member_call_points_e2e.rs"]
mod suspend_member_call_points_e2e;
#[path = "../suspend_normalization_e2e.rs"]
mod suspend_normalization_e2e;
#[path = "../suspend_operator_convention_cross_file_e2e.rs"]
mod suspend_operator_convention_cross_file_e2e;
#[path = "../suspend_private_function_e2e.rs"]
mod suspend_private_function_e2e;
#[path = "../suspend_prologue_cast_e2e.rs"]
mod suspend_prologue_cast_e2e;
#[path = "../suspend_receiver_lambda_e2e.rs"]
mod suspend_receiver_lambda_e2e;
#[path = "../suspend_reified_inline_lambda_e2e.rs"]
mod suspend_reified_inline_lambda_e2e;
#[path = "../suspend_result_nullability_e2e.rs"]
mod suspend_result_nullability_e2e;
#[path = "../suspend_result_operands_e2e.rs"]
mod suspend_result_operands_e2e;
#[path = "../suspend_return_type_recovery_e2e.rs"]
mod suspend_return_type_recovery_e2e;
#[path = "../suspend_safe_call_receiver_e2e.rs"]
mod suspend_safe_call_receiver_e2e;
#[path = "../suspend_single_operand_hoist_e2e.rs"]
mod suspend_single_operand_hoist_e2e;
#[path = "../suspend_spill_slot_reuse_e2e.rs"]
mod suspend_spill_slot_reuse_e2e;
#[path = "../suspend_statement_when_block_arms_e2e.rs"]
mod suspend_statement_when_block_arms_e2e;
#[path = "../suspend_super_call_refusal_e2e.rs"]
mod suspend_super_call_refusal_e2e;
#[path = "../suspend_super_calls_e2e.rs"]
mod suspend_super_calls_e2e;
#[path = "../suspend_throw_operand_e2e.rs"]
mod suspend_throw_operand_e2e;
#[path = "../suspend_try_catch_shapes_e2e.rs"]
mod suspend_try_catch_shapes_e2e;
#[path = "../suspend_try_finally_body_e2e.rs"]
mod suspend_try_finally_body_e2e;
#[path = "../suspend_try_finally_e2e.rs"]
mod suspend_try_finally_e2e;
#[path = "../suspend_under_try_e2e.rs"]
mod suspend_under_try_e2e;
#[path = "../suspend_unit_receiver_e2e.rs"]
mod suspend_unit_receiver_e2e;
#[path = "../suspend_unit_tail_conditional_e2e.rs"]
mod suspend_unit_tail_conditional_e2e;
#[path = "../suspend_value_class_carrier_rebox_e2e.rs"]
mod suspend_value_class_carrier_rebox_e2e;
#[path = "../suspend_value_class_delegation_e2e.rs"]
mod suspend_value_class_delegation_e2e;
#[path = "../suspend_value_class_mangle_e2e.rs"]
mod suspend_value_class_mangle_e2e;
#[path = "../suspend_value_class_results_e2e.rs"]
mod suspend_value_class_results_e2e;
#[path = "../suspend_value_invoke_e2e.rs"]
mod suspend_value_invoke_e2e;
#[path = "../suspend_when_branch_resume_line_e2e.rs"]
mod suspend_when_branch_resume_line_e2e;
#[path = "../suspend_when_branch_spill_e2e.rs"]
mod suspend_when_branch_spill_e2e;
#[path = "../suspend_withlock_nonlocal_return_e2e.rs"]
mod suspend_withlock_nonlocal_return_e2e;
