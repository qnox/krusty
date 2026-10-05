//! Constructor declarations: the primary constructor's header prefix and secondary constructors.
//!
//! A class body and an enum body reach the same `constructor(…)` syntax by different routes, and
//! each used to parse it separately — two copies that had already drifted in spelling and would
//! drift in behaviour the moment one of them learned something new.

use super::*;

/// A class header's primary-constructor prefix — `[annotations] [modifiers] [constructor]` — up to
/// its parameter list.
pub(super) struct PrimaryCtorPrefix {
    pub(super) visibility: Visibility,
    pub(super) annotations: Vec<AnnotationRef>,
    pub(super) annotation_args: Vec<Vec<ExprId>>,
    /// Whether the header wrote the `constructor` keyword.
    pub(super) keyword: bool,
    /// The OFFSET the constructor itself starts at: its keyword, or the token after the prefix —
    /// its `(` — when it wrote none. Meaningful only when the header declares a constructor.
    pub(super) start: u32,
}

impl Parser<'_> {
    /// Parse the primary-constructor prefix after a class name and its type parameters. The
    /// constructor's own start skips its annotations and modifiers, exactly as a secondary
    /// constructor's `decl_line` is its `constructor` keyword.
    pub(super) fn parse_primary_constructor_prefix(&mut self) -> PrimaryCtorPrefix {
        let mut prefix = PrimaryCtorPrefix {
            visibility: Visibility::Public,
            annotations: Vec::new(),
            annotation_args: Vec::new(),
            keyword: false,
            start: 0,
        };
        // An explicit constructor prefix may continue across physical newlines.
        if self.primary_constructor_header_follows() {
            self.skip_newlines();
            if self.at(TokenKind::At) || self.at_modifier() {
                let modifiers = self.skip_decl_prefix();
                // The declared constructor visibility survives into `@Metadata` (and, for
                // `protected`, the JVM `<init>` access flags).
                prefix.visibility = visibility_of(&modifiers);
                prefix.annotations = self.take_pending_annotations();
                prefix.annotation_args = self.take_pending_annotation_args();
            }
        }
        prefix.start = self.tok().span.lo;
        prefix.keyword = self.at(TokenKind::Ident) && self.keyword_text("constructor");
        if prefix.keyword {
            self.bump();
        }
        prefix
    }

    /// Parse one `constructor(params) [: this(args) | : super(args)] [{ body }]`, positioned on the
    /// `constructor` keyword. Pending annotations belong to it and are taken here.
    pub(super) fn parse_secondary_constructor(&mut self, modifiers: &[String]) -> SecondaryCtor {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let keyword = self.tok().span;
        // The modifier tokens were consumed before this parser ran, so the declaration's own start
        // is the earliest one of them; a constructor with no modifiers starts at its keyword.
        let start = modifiers
            .iter()
            .filter_map(|modifier| declaration_modifiers::span(self, modifiers, modifier))
            .map(|span| span.lo)
            .min()
            .unwrap_or(keyword.lo);
        self.bump(); // 'constructor'
        let params = self.parse_param_list();
        let mut delegation = CtorDelegation::None;
        let mut delegation_offset = 0;
        if self.eat(TokenKind::Colon) {
            self.skip_newlines();
            let target = if self.at(TokenKind::Ident) {
                let target = self.text().to_string();
                delegation_offset = self.tok().span.lo;
                self.bump();
                target
            } else {
                String::new()
            };
            let (args, names) = self.parse_call_arguments_with_names();
            let call = CtorDelegationCall {
                args,
                names,
                trailing_lambda: false,
            };
            delegation = match target.as_str() {
                "this" => CtorDelegation::This(call),
                "super" => CtorDelegation::Super(call),
                _ => {
                    self.diags.error(
                        keyword,
                        "expected 'this' or 'super' in constructor delegation",
                    );
                    CtorDelegation::None
                }
            };
        }
        // A bodyless constructor ends at its delegation, not at the newlines skipped after it.
        let mut end = self.t[self.i.saturating_sub(1)].span.hi;
        self.skip_newlines();
        let body = self
            .at(TokenKind::LBrace)
            .then(|| self.parse_block_expr(false));
        if body.is_some() {
            end = self.t[self.i.saturating_sub(1)].span.hi;
        }
        SecondaryCtor {
            annotations,
            annotation_args,
            params,
            delegation,
            body,
            span: Span::new(keyword.lo, end),
            is_actual: modifiers.iter().any(|modifier| modifier == "actual"),
            declaration_span: Span::new(start, end),
            // The debug-line post-pass rewrites these to their lines, the way it rewrites every
            // other declaration's; the delegation carries its OFFSET until then.
            decl_line: 0,
            delegation_line: delegation_offset,
            decl_end_line: 0,
            default_lines: Vec::new(),
            visibility: visibility_of(modifiers),
        }
    }
}
