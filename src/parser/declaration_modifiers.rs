//! Source locations retained for declaration modifiers with modifier-owned diagnostics.

use super::Parser;
use crate::diag::Span;

/// Return the source span of `modifier` when it was present in the parsed modifier set.
///
/// Declaration parsing consumes modifier tokens before the declaration-specific parser runs. Keep
/// the reverse token lookup in one place so every diagnostic that belongs to a modifier points at
/// that modifier rather than independently reconstructing its location.
pub(super) fn span(parser: &Parser<'_>, modifiers: &[String], modifier: &str) -> Option<Span> {
    modifiers
        .iter()
        .any(|candidate| candidate == modifier)
        .then(|| {
            parser.t[..parser.i]
                .iter()
                .rev()
                .find(|token| parser.token_keyword_text(**token, modifier))
                .map(|token| token.span)
                .unwrap_or_else(|| panic!("a {modifier} modifier must retain its source token"))
        })
}
