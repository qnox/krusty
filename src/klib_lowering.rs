//! Lowering of decoded KLIB IR bodies into checked common IR.
//!
//! A selected dependency callable is joined to its serialized body by the exact signature its
//! provider published ([`crate::klib_libraries::KlibDeclarationBodies`]), and the body is lowered
//! into the same common IR checked FIR lowering produces for equivalent source, so a backend
//! compiles it as it compiles a module function. Nothing here names a target: Native and Wasm
//! share it. A body using a form this lowering does not model declines by the form's name.

mod body_lowering;
mod body_unit;
mod decline;
mod function_lowering;
mod ir_builtins;
mod klib_types;
#[cfg(test)]
mod tests;

pub use crate::libraries::KlibBodyCallable as KlibCallable;
pub use body_unit::DependencyBodyUnit;
pub use decline::{KlibBodyDecline, KlibBodyDeclineReason};
