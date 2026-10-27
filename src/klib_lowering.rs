//! Lowering of decoded KLIB IR bodies into checked common IR.
//!
//! A selected dependency callable is joined to its serialized body by the exact module-qualified
//! signature its
//! provider published ([`crate::klib_libraries::KlibDeclarationBodies`]), and the body is lowered
//! into the same common IR checked FIR lowering produces for equivalent source, so a backend
//! compiles it as it compiles a module function. A body's calls follow the serialized `IdSignature`
//! kotlinc resolved, never re-selecting an overload: each callee is joined to its serialized
//! declaration by that identity, declared from its header (cross-checked against the provider
//! record a checked call selected, [`KlibCalleeFacts`], when there is one), and its body lowered
//! into the same unit. Nothing here names a target: Native and Wasm share it. A body using a form
//! this lowering does not model declines by the form's name.

mod body_lowering;
mod body_unit;
mod callee_facts;
mod class_headers;
mod class_initialization;
mod closure_forms;
mod constructor_bounds;
mod constructor_calls;
mod decline;
mod function_lowering;
mod inline_templates;
mod ir_builtins;
mod klib_types;
mod local_values;
mod lowering_checkpoint;
mod primitive_calls;
mod range_membership;
#[cfg(test)]
mod tests;

pub use crate::libraries::KlibBodyCallable as KlibCallable;
pub use body_unit::DependencyBodyUnit;
pub(crate) use body_unit::EmittedDependencyCallable;
pub use callee_facts::KlibCalleeFacts;
pub(crate) use closure_forms::emitted_closure_function;

/// The serialized forms common body lowering realizes without the declaration edges they name.
pub(crate) const EMITTED_CALL_FORMS: crate::metadata::klib_ir::tree::EmittedCallForms =
    crate::metadata::klib_ir::tree::EmittedCallForms {
        closure: emitted_closure_function,
        folded_call: range_membership::folds_range_literal_membership,
    };
pub use decline::{KlibBodyDecline, KlibBodyDeclineReason};
pub use inline_templates::{lower_inline_template, KlibInlineTemplate, TemplateCallees};
