//! The C declaration parser: the preprocessor's tokens to the file-scope [`Declarations`] of a
//! header set, with every type laid out for the target.
//!
//! It reads what C11 headers and their GNU extensions declare: typedefs, functions (variadic,
//! old-style, with function-pointer parameters), struct and union definitions with bitfields and
//! anonymous members, enums whose values are constant expressions, and global variables; with
//! `__attribute__((…))`, `__extension__`, `__asm__("name")` symbol labels, `__restrict`,
//! `__inline`, `_Noreturn`, `_Alignas`, `_Atomic` and `__typeof__`. A function body — a header's
//! `static inline` helper — is skipped, and the function is recorded as having one, since
//! Kotlin/Native binds only functions with an external definition.
//!
//! Parsing needs the target: a constant expression (`char pad[sizeof (long) * 2]`) may measure a
//! type, and the header itself was preprocessed for that target. The grammar is split by
//! production: [`specifiers`] (declaration specifiers, struct, union and enum bodies),
//! [`declarators`] (declarators, parameters and type names), [`attributes`] (GNU attributes and
//! asm labels) and [`constant`] (integer constant expressions), all over the token cursor and
//! file scope that [`parser`] owns.

mod attributes;
mod constant;
mod declarators;
mod parser;
mod specifiers;

use super::layout::LayoutEngine;
use super::lexer::Token;
use super::model::Declarations;
use crate::native::target::NativeTarget;

/// A declaration the parser could not read, with where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParseError {
    pub(super) file: String,
    pub(super) line: u32,
    pub(super) message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}: {}", self.file, self.line, self.message)
    }
}

/// A parsed header set: its declarations and the target's layout of their types.
pub(super) struct Header {
    pub(super) declarations: Declarations,
    pub(super) layouts: LayoutEngine,
}

/// Parse the preprocessed `tokens` of one translation unit for `target`.
pub(super) fn parse(tokens: &[Token], target: NativeTarget) -> Result<Header, ParseError> {
    let mut parser = parser::Parser::new(tokens, target);
    parser.translation_unit()?;
    Ok(parser.finish())
}

#[cfg(test)]
mod tests;
