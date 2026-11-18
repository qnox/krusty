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
    ///
    /// Under `+ExplicitInlineValueClassesSyntax` an `inline value class` declares itself inline,
    /// exactly as a legacy `inline class` does; its shape is reported at its first keyword.
    pub fn value_class_keyword(&self, file: &File) -> Option<ValueClassKeyword> {
        let inline_span = || {
            let prefix = file.declaration_prefixes.at(self.name_span.lo)?;
            file.declaration_prefixes
                .keywords(prefix)
                .iter()
                .find(|(keyword, _)| *keyword == ModifierKeyword::Inline)
                .map(|(_, span)| *span)
        };
        if let Some(value_span) = self.value_modifier_span {
            let explicit_inline = file
                .language
                .explicit_inline_value_classes
                .then(inline_span)
                .flatten();
            return Some(match explicit_inline {
                Some(inline_span) => ValueClassKeyword {
                    span: if inline_span.lo <= value_span.lo {
                        inline_span
                    } else {
                        value_span
                    },
                    declares_inline: true,
                },
                None => ValueClassKeyword {
                    span: value_span,
                    declares_inline: false,
                },
            });
        }
        if !self.is_value {
            return None;
        }
        Some(ValueClassKeyword {
            span: inline_span()?,
            declares_inline: true,
        })
    }

    /// This declaration is a value class inline by its modifiers alone.
    pub fn declares_inline_value_class(&self, file: &File) -> bool {
        self.value_class_keyword(file)
            .is_some_and(|keyword| keyword.declares_inline)
    }
}
