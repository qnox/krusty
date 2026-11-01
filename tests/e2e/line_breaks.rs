//! Source line breaks inside expressions and declarations: parsing them, and the line numbers
//! kotlinc records for the code on either side of them.

use super::common;

#[path = "../cast_line_break_e2e.rs"]
mod cast_line_break_e2e;
#[path = "../class_header_newline_e2e.rs"]
mod class_header_newline_e2e;
#[path = "../decl_body_on_next_line_e2e.rs"]
mod decl_body_on_next_line_e2e;
#[path = "../elvis_newline_continuation_e2e.rs"]
mod elvis_newline_continuation_e2e;
#[path = "../infix_newline_operand_e2e.rs"]
mod infix_newline_operand_e2e;
#[path = "../multiline_receiver_access_line_e2e.rs"]
mod multiline_receiver_access_line_e2e;
#[path = "../newline_method_chain_e2e.rs"]
mod newline_method_chain_e2e;
#[path = "../paren_condition_newline_e2e.rs"]
mod paren_condition_newline_e2e;
#[path = "../parenthesized_line_breaks_e2e.rs"]
mod parenthesized_line_breaks_e2e;
#[path = "../primary_constructor_newline_e2e.rs"]
mod primary_constructor_newline_e2e;
#[path = "../property_initializer_newline_e2e.rs"]
mod property_initializer_newline_e2e;
#[path = "../receiver_type_line_break_e2e.rs"]
mod receiver_type_line_break_e2e;
#[path = "../serializable_default_own_lines_e2e.rs"]
mod serializable_default_own_lines_e2e;
#[path = "../type_annotation_newline_e2e.rs"]
mod type_annotation_newline_e2e;
