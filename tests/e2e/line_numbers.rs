//! The line numbers kotlinc records for statements and the values they produce: where a local's
//! initializer, a call, a comparison jump, a loop transfer or an implicit return is marked.

use super::common;

#[path = "../also_apply_result_line_e2e.rs"]
mod also_apply_result_line_e2e;
#[path = "../comparison_jump_line_e2e.rs"]
mod comparison_jump_line_e2e;
#[path = "../constant_loop_condition_line_e2e.rs"]
mod constant_loop_condition_line_e2e;
#[path = "../elvis_own_line_e2e.rs"]
mod elvis_own_line_e2e;
#[path = "../expression_line_marks_e2e.rs"]
mod expression_line_marks_e2e;
#[path = "../fallthrough_close_line_e2e.rs"]
mod fallthrough_close_line_e2e;
#[path = "../generic_call_line_marks_e2e.rs"]
mod generic_call_line_marks_e2e;
#[path = "../getter_return_line_e2e.rs"]
mod getter_return_line_e2e;
#[path = "../lambda_implicit_return_lines_e2e.rs"]
mod lambda_implicit_return_lines_e2e;
#[path = "../loop_control_lines_e2e.rs"]
mod loop_control_lines_e2e;
#[path = "../loop_transfer_line_e2e.rs"]
mod loop_transfer_line_e2e;
#[path = "../safe_call_null_line_e2e.rs"]
mod safe_call_null_line_e2e;
#[path = "../safe_call_return_line_e2e.rs"]
mod safe_call_return_line_e2e;
#[path = "../setter_return_line_e2e.rs"]
mod setter_return_line_e2e;
#[path = "../unit_expression_body_return_line_e2e.rs"]
mod unit_expression_body_return_line_e2e;
#[path = "../variable_initializer_line_e2e.rs"]
mod variable_initializer_line_e2e;
