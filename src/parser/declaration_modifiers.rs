//! Source locations retained for declaration modifiers with modifier-owned diagnostics.

use super::{Parser, TokenKind};
use crate::ast::{AnnotationRef, ExprId};
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

    /// Consume one annotation and retain its complete classifier reference plus arguments. `None` for
    /// a use-site `@file:`/`@get:` target, which does not apply to the declaration/type parameter itself.
    pub(super) fn parse_annotation(&mut self) -> (Option<AnnotationRef>, Vec<ExprId>) {
        self.bump(); // '@'
                     // optional use-site target: `file:`, `get:`, `param:`, ...
        let mut use_site = false;
        let mut target = String::new();
        if self.at(TokenKind::Ident)
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::Colon)
        {
            target = self.text().to_string();
            self.bump();
            self.bump(); // ':'
            use_site = true;
        }
        // Kotlin's grouped file-target form omits `@file:` from each entry:
        // `@file:[JvmName("Facade") JvmMultifileClass]`. Each entry is still an independent file
        // annotation with its own reference and arguments. Consume the group at the annotation
        // grammar boundary so the following `package` remains an ordinary file directive.
        if target == "file" && self.at(TokenKind::LBracket) {
            self.bump(); // '['
            loop {
                self.skip_newlines();
                if self.at(TokenKind::RBracket) || self.at(TokenKind::Eof) {
                    break;
                }
                let (qname, annotation_span) = self.parse_annotation_reference();
                let args = self.parse_annotation_args();
                if !qname.is_empty() {
                    self.file.file_annotations.push((
                        AnnotationRef {
                            name: qname,
                            span: annotation_span,
                        },
                        args,
                    ));
                }
                if self.at(TokenKind::Comma) {
                    self.bump();
                }
            }
            self.eat(TokenKind::RBracket);
            return (None, Vec::new());
        }
        let (qname, annotation_span) = self.parse_annotation_reference();
        let args = self.parse_annotation_args();
        if target == "file" && !qname.is_empty() {
            self.file.file_annotations.push((
                AnnotationRef {
                    name: qname.clone(),
                    span: annotation_span,
                },
                args.clone(),
            ));
        }
        if use_site || qname.is_empty() {
            (None, args)
        } else {
            (
                Some(AnnotationRef {
                    name: qname,
                    span: annotation_span,
                }),
                args,
            )
        }
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

    /// Consume another modifier/annotation run belonging to the declaration whose prefix is
    /// already buffered. Kotlin permits context and `companion` clauses between such runs, so a
    /// later run must append annotations rather than erase the ones parsed before the clause.
    pub(super) fn extend_decl_prefix(&mut self) -> Vec<String> {
        let mut annotations = std::mem::take(&mut self.pending_annotations);
        let mut annotation_args = std::mem::take(&mut self.pending_annotation_args);
        let modifiers = self.skip_decl_prefix();
        annotations.append(&mut self.pending_annotations);
        annotation_args.append(&mut self.pending_annotation_args);
        self.pending_annotations = annotations;
        self.pending_annotation_args = annotation_args;
        modifiers
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

/// Record a nested classifier that wrote `actual` as its own actualization target.
///
/// The enclosing declaration's `actual` does not cover it: the reference compiler reports every
/// unmatched `actual` at its own name, a nested classifier's included. The top-level record keeps
/// only the outermost declaration of a hoisted group, because a hoisted descendant carries the
/// modifier only if it wrote one itself — which is knowable here, at the two sites that own the
/// nested declaration's modifier list, and nowhere afterwards without pairing a keyword span with
/// whichever declaration happens to follow it.
pub(super) fn record_nested_actual(
    file: &mut crate::ast::File,
    modifiers: &[String],
    nested: crate::ast::DeclId,
) {
    if modifiers.iter().any(|modifier| modifier == "actual") {
        file.actual_decls.push(nested);
    }
}
