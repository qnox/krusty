//! What a declaration's modifier list wrote, retained for checks that read the list itself.
//!
//! Visibility on the AST is a resolved value: an unmodified declaration is `public`, the same as
//! one that wrote `public`. Explicit API mode distinguishes the two, and reports at the start of
//! the modifier list rather than at the name. Neither fact survives on the declaration, so the
//! parser records both where it consumes the list.

use crate::diag::Span;
use crate::fx_hash::HashMap;
use crate::types::Visibility;

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
    /// Every modifier keyword the list wrote, a classifier's kind words (`data`, `enum`,
    /// `annotation`, `companion`, `fun`) included.
    pub modifiers: WrittenModifiers,
}

/// A keyword a declaration's modifier list can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierKeyword {
    Public,
    Private,
    Internal,
    Protected,
    Open,
    Final,
    Abstract,
    Sealed,
    Inline,
    Noinline,
    Crossinline,
    Operator,
    Override,
    Suspend,
    Tailrec,
    Lateinit,
    Infix,
    Reified,
    Vararg,
    Const,
    Actual,
    Expect,
    Value,
    Inner,
    External,
    Data,
    Enum,
    Annotation,
    Companion,
    Fun,
}

impl ModifierKeyword {
    pub const ALL: [ModifierKeyword; 30] = [
        ModifierKeyword::Public,
        ModifierKeyword::Private,
        ModifierKeyword::Internal,
        ModifierKeyword::Protected,
        ModifierKeyword::Open,
        ModifierKeyword::Final,
        ModifierKeyword::Abstract,
        ModifierKeyword::Sealed,
        ModifierKeyword::Inline,
        ModifierKeyword::Noinline,
        ModifierKeyword::Crossinline,
        ModifierKeyword::Operator,
        ModifierKeyword::Override,
        ModifierKeyword::Suspend,
        ModifierKeyword::Tailrec,
        ModifierKeyword::Lateinit,
        ModifierKeyword::Infix,
        ModifierKeyword::Reified,
        ModifierKeyword::Vararg,
        ModifierKeyword::Const,
        ModifierKeyword::Actual,
        ModifierKeyword::Expect,
        ModifierKeyword::Value,
        ModifierKeyword::Inner,
        ModifierKeyword::External,
        ModifierKeyword::Data,
        ModifierKeyword::Enum,
        ModifierKeyword::Annotation,
        ModifierKeyword::Companion,
        ModifierKeyword::Fun,
    ];

    /// The modifier `keyword` spells, if it spells one.
    pub fn from_keyword(keyword: &str) -> Option<ModifierKeyword> {
        ModifierKeyword::ALL
            .into_iter()
            .find(|modifier| modifier.keyword() == keyword)
    }

    pub fn keyword(self) -> &'static str {
        match self {
            ModifierKeyword::Public => "public",
            ModifierKeyword::Private => "private",
            ModifierKeyword::Internal => "internal",
            ModifierKeyword::Protected => "protected",
            ModifierKeyword::Open => "open",
            ModifierKeyword::Final => "final",
            ModifierKeyword::Abstract => "abstract",
            ModifierKeyword::Sealed => "sealed",
            ModifierKeyword::Inline => "inline",
            ModifierKeyword::Noinline => "noinline",
            ModifierKeyword::Crossinline => "crossinline",
            ModifierKeyword::Operator => "operator",
            ModifierKeyword::Override => "override",
            ModifierKeyword::Suspend => "suspend",
            ModifierKeyword::Tailrec => "tailrec",
            ModifierKeyword::Lateinit => "lateinit",
            ModifierKeyword::Infix => "infix",
            ModifierKeyword::Reified => "reified",
            ModifierKeyword::Vararg => "vararg",
            ModifierKeyword::Const => "const",
            ModifierKeyword::Actual => "actual",
            ModifierKeyword::Expect => "expect",
            ModifierKeyword::Value => "value",
            ModifierKeyword::Inner => "inner",
            ModifierKeyword::External => "external",
            ModifierKeyword::Data => "data",
            ModifierKeyword::Enum => "enum",
            ModifierKeyword::Annotation => "annotation",
            ModifierKeyword::Companion => "companion",
            ModifierKeyword::Fun => "fun",
        }
    }
}

/// A set of modifier keywords, in [`ModifierKeyword::ALL`] order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WrittenModifiers(u32);

impl WrittenModifiers {
    pub(crate) fn insert(&mut self, modifier: ModifierKeyword) {
        self.0 |= 1 << modifier as u32;
    }

    pub fn contains(self, modifier: ModifierKeyword) -> bool {
        self.0 & (1 << modifier as u32) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = ModifierKeyword> {
        ModifierKeyword::ALL
            .into_iter()
            .filter(move |modifier| self.contains(*modifier))
    }
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
