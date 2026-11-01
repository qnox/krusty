//! Implicit-receiver call tower integration coverage.

use super::common;

#[path = "../context_argument_line_marks_e2e.rs"]
mod context_argument_line_marks;
#[path = "../implicit_receiver_extension_tower_e2e.rs"]
mod extension_tower;
#[path = "../implicit_receiver_inapplicable_call_e2e.rs"]
mod inapplicable_call;
#[path = "../implicit_receiver_member_shadow_e2e.rs"]
mod member_shadow;
#[path = "../implicit_receiver_named_args_e2e.rs"]
mod named_args;
#[path = "../implicit_receiver_tower_lambda_shape_e2e.rs"]
mod tower_lambda_shape;
