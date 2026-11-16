//! How a classifier declaration writes itself a value class.

use super::{ClassDecl, File, ModifierKeyword};
use crate::diag::Span;

/// The modifier that makes a declaration a value class, and whether it declares the class inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueClassKeyword {
    /// Where kotlinc reports the declaration's value-class shape: the `value` keyword of a
    /// `value class`, the `inline` keyword of a legacy `inline class`.
    pub span: Span,
    /// The declaration is inline by its modifiers alone, as a `@JvmInline value class` is by its
    /// annotation. A `value class` is not: its annotations and language features decide.
    pub declares_inline: bool,
}

impl ClassDecl {
    /// The keyword that makes this declaration a value class, when it is one.
    pub fn value_class_keyword(&self, file: &File) -> Option<ValueClassKeyword> {
        if let Some(span) = self.value_modifier_span {
            return Some(ValueClassKeyword {
                span,
                declares_inline: false,
            });
        }
        if !self.is_value {
            return None;
        }
        let prefix = file.declaration_prefixes.at(self.name_span.lo)?;
        let span = file
            .declaration_prefixes
            .keywords(prefix)
            .iter()
            .find(|(keyword, _)| *keyword == ModifierKeyword::Inline)?
            .1;
        Some(ValueClassKeyword {
            span,
            declares_inline: true,
        })
    }

    /// This declaration is a value class inline by its modifiers alone.
    pub fn declares_inline_value_class(&self, file: &File) -> bool {
        self.value_class_keyword(file)
            .is_some_and(|keyword| keyword.declares_inline)
    }
}
