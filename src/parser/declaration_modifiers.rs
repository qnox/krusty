//! Source locations retained for declaration modifiers with modifier-owned diagnostics.

use super::{Parser, TokenKind};
use crate::diag::Span;

impl Parser<'_> {
    /// Consume leading annotations (`@Foo`, `@file:Bar(...)`) and soft modifiers (`public`, `open`,
    /// `inline`, `operator`, `suspend`, …) that precede a declaration. Modifiers that change the
    /// declaration *kind* (`enum`, `annotation`, `data`, `object`, …) are left for their real
    /// declaration productions rather than being mistaken for ordinary modifiers.
    /// Take the annotations captured by the preceding `skip_decl_prefix`, clearing the buffer.
    /// `parse_class`/`parse_enum`/… call this FIRST so member-prefix parsing doesn't clobber them.
    pub(super) fn take_pending_annotations(&mut self) -> Vec<crate::ast::AnnotationRef> {
        std::mem::take(&mut self.pending_annotations)
    }

    /// Take the per-annotation argument expressions captured by the preceding `skip_decl_prefix`
    /// (parallel to [`Parser::take_pending_annotations`]), clearing the buffer.
    pub(super) fn take_pending_annotation_args(&mut self) -> Vec<Vec<crate::ast::ExprId>> {
        std::mem::take(&mut self.pending_annotation_args)
    }

    pub(super) fn skip_decl_prefix(&mut self) -> Vec<String> {
        let mut mods = Vec::new();
        self.pending_annotations.clear();
        self.pending_annotation_args.clear();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::At) {
                let (name, args) = self.parse_annotation();
                if let Some(name) = name {
                    self.pending_annotations.push(name);
                    self.pending_annotation_args.push(args);
                }
            } else if self.at_modifier()
                && self.t.get(self.i + 1).map(|t| t.kind) != Some(TokenKind::Colon)
            {
                // A modifier soft keyword immediately followed by `:` is a NAME, not a modifier
                // (`fun f(open: Int)`, `@Anno sealed: T`) — a real modifier is never followed by a colon.
                let text = self.text().to_string();
                // `expect` and `actual` are legal only in a multiplatform project, and kotlinc's
                // diagnostic points at the MODIFIER, so its span is captured here where the keyword
                // is in hand. Every call site records, members included, because a member `actual`
                // is reported too. Deduped by span: the `companion fun` lookahead consumes a prefix
                // and rewinds, and the ordinary path then re-consumes the same keyword.
                if text == "expect" || text == "actual" {
                    let span = self.tok().span;
                    if !self
                        .file
                        .multiplatform_modifiers
                        .iter()
                        .any(|(_, seen)| *seen == span)
                    {
                        self.file.multiplatform_modifiers.push((text.clone(), span));
                    }
                }
                mods.push(text);
                self.bump();
            } else {
                break;
            }
        }
        mods
    }
}

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
