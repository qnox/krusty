//! Source locations retained for declaration modifiers with modifier-owned diagnostics.

use super::{Parser, TokenKind};
use crate::ast::{AnnotationRef, DeclarationPrefix, ExprId};
use crate::diag::Span;
use crate::types::Visibility;

/// The modifier list of the declaration being parsed, accumulated across its `skip_decl_prefix`
/// runs (a context clause or `companion` may separate them) until the declaration records it.
#[derive(Default)]
pub(super) struct PrefixInProgress {
    first_modifier: Option<Span>,
    visibility: Option<Visibility>,
}

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
    /// Keep a type-use annotation's non-empty argument list, keyed by the annotation's span.
    pub(super) fn record_type_annotation_arguments(
        &mut self,
        annotation: Span,
        arguments: Vec<ExprId>,
    ) {
        if !arguments.is_empty() {
            let key = (annotation.lo, annotation.hi);
            self.file.type_annotation_arguments.insert(key, arguments);
        }
    }

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
                    let annotation = AnnotationRef {
                        name: qname,
                        span: annotation_span,
                    };
                    self.file.file_annotations.push((annotation, args));
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
            let annotation = AnnotationRef {
                name: qname.clone(),
                span: annotation_span,
            };
            self.file.file_annotations.push((annotation, args.clone()));
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
                self.note_declaration_prefix_token(self.t[self.i].span);
                let prefix = &mut self.declaration_prefix;
                if matches!(
                    text.as_str(),
                    "public" | "protected" | "internal" | "private"
                ) {
                    prefix
                        .visibility
                        .get_or_insert(Visibility::from_modifier(&text));
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

impl Parser<'_> {
    /// Start a declaration whose modifier list [`Parser::record_declaration_prefix`] will keep.
    pub(super) fn begin_declaration_prefix(&mut self) {
        self.declaration_prefix = PrefixInProgress::default();
    }

    /// Note a token of the modifier list being consumed; the first one starts the list as kotlinc
    /// reports it. Annotations are not noted: a report about the modifiers starts after them.
    pub(super) fn note_declaration_prefix_token(&mut self, token: Span) {
        self.declaration_prefix.first_modifier.get_or_insert(token);
    }

    /// Keep the modifier list consumed since [`Parser::begin_declaration_prefix`] for the
    /// declaration starting at `start`, positioned on the token after that list. A classifier's
    /// kind words (`data`, `enum`, `annotation`, `companion`, `fun interface`) are not modifiers to
    /// the parser, so its keyword and name are found past them; each is a key the declaration can
    /// find its prefix by.
    pub(super) fn record_declaration_prefix(&mut self, start: u32) {
        let prefix = std::mem::take(&mut self.declaration_prefix);
        let after = self.tok().span;
        let mut keys = vec![start, after.lo];
        let mut index = self.i;
        while self.t.get(index).is_some_and(|token| {
            ["data", "enum", "annotation", "companion"]
                .iter()
                .any(|word| self.token_keyword_text(*token, word))
                || (token.kind == TokenKind::KwFun
                    && self
                        .t
                        .get(index + 1)
                        .is_some_and(|next| self.token_keyword_text(*next, "interface")))
        }) {
            index += 1;
        }
        if let Some(keyword) = self.t.get(index).filter(|token| {
            token.kind == TokenKind::KwClass
                || self.token_keyword_text(**token, "object")
                || self.token_keyword_text(**token, "interface")
        }) {
            keys.push(keyword.span.lo);
            if let Some(name) = self.t.get(index + 1).filter(|t| t.kind == TokenKind::Ident) {
                keys.push(name.span.lo);
            }
        }
        self.file.declaration_prefixes.record(
            &keys,
            DeclarationPrefix {
                start: prefix.first_modifier.unwrap_or(after),
                visibility: prefix.visibility,
            },
        );
    }
}

/// The function flags a modifier list declares.
pub(super) fn function_flags(modifiers: &[String]) -> crate::ast::FdFlags {
    let mut flags = crate::ast::FdFlags::default()
        .with_has_visibility_modifier(has_visibility_modifier(modifiers));
    let mut is_final = false;
    let mut is_open = false;
    for modifier in modifiers {
        flags = match modifier.as_str() {
            "inline" => flags.with_is_inline(true),
            "final" => {
                is_final = true;
                flags.with_is_final(true)
            }
            "open" => {
                is_open = true;
                flags
            }
            "override" => {
                is_open = true;
                flags.with_is_override(true)
            }
            "abstract" => flags.with_is_abstract(true),
            "suspend" => flags.with_is_suspend(true),
            "tailrec" => flags.with_is_tailrec(true),
            "operator" => flags.with_is_operator(true),
            "infix" => flags.with_is_infix(true),
            "companion" => flags.with_is_companion_extension(true),
            "external" => flags.with_is_external(true),
            "actual" => flags.with_is_actual(true),
            _ => flags,
        };
    }
    flags.with_is_open(is_open && !is_final)
}

/// The visibility a modifier list declares; a declaration that writes none is `public`.
pub(super) fn visibility_of(modifiers: &[String]) -> Visibility {
    modifiers
        .iter()
        .find(|m| matches!(m.as_str(), "private" | "protected" | "internal" | "public"))
        .map(|m| Visibility::from_modifier(m))
        .unwrap_or_default()
}

/// Whether the modifier list wrote a visibility keyword. An `override` with none keeps the
/// overridden member's visibility rather than defaulting to `public`.
pub(super) fn has_visibility_modifier(modifiers: &[String]) -> bool {
    modifiers
        .iter()
        .any(|m| matches!(m.as_str(), "private" | "protected" | "internal" | "public"))
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
