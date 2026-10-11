//! What a declaration's modifier list wrote, retained for checks that read the list itself.
//!
//! Visibility on the AST is a resolved value: an unmodified declaration is `public`, the same as
//! one that wrote `public`. Explicit API mode distinguishes the two, and reports at the start of
//! the modifier list rather than at the name. Neither fact survives on the declaration, so the
//! parser records both where it consumes the list.

use crate::diag::Span;
use crate::types::Visibility;
use std::collections::HashMap;

/// One declaration's modifier list, as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclarationPrefix {
    /// The first modifier keyword after any annotations, or the declaration's own keyword when it
    /// wrote no modifier. kotlinc starts a diagnostic about the declaration's modifiers here.
    pub start: Span,
    /// The visibility modifier the declaration wrote, if any.
    pub visibility: Option<Visibility>,
    /// Where the whole declaration starts, annotations included.
    pub declaration_start: u32,
    /// A classifier's `class`, `object` or `interface` keyword, when the declaration has one.
    pub classifier_keyword: Option<Span>,
}

/// Declaration prefixes keyed by the offsets of the tokens that identify their declaration: the
/// prefix's own start, its declaration keyword, and a classifier's name. Each declaration kind
/// finds its prefix through an offset it already carries.
#[derive(Default)]
pub struct DeclarationPrefixes {
    by_offset: HashMap<u32, DeclarationPrefix>,
}

impl DeclarationPrefixes {
    pub(crate) fn record(&mut self, offsets: &[u32], prefix: DeclarationPrefix) {
        for &offset in offsets {
            self.by_offset.insert(offset, prefix);
        }
    }

    pub fn at(&self, offset: u32) -> Option<DeclarationPrefix> {
        self.by_offset.get(&offset).copied()
    }
}
