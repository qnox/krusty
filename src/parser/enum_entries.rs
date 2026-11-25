use super::*;

impl Parser<'_> {
    /// Parses the entry list of an enum body up to the `;` that starts its members.
    pub(super) fn parse_enum_entries(&mut self, name: &str) -> Vec<AstEnumEntry> {
        let mut entries: Vec<AstEnumEntry> = Vec::new();
        loop {
            let previous_end = self
                .i
                .checked_sub(1)
                .map_or(0, |previous| self.t[previous].span.hi);
            let entry_start = comment_binding::leading_start(
                self.src,
                previous_end,
                self.syntactic_ident_span(self.tok()).lo,
            );
            let (entry_ann_names, entry_ann_args) = self.parse_enum_entry_prefix();
            if !self.at(TokenKind::Ident) {
                break;
            }
            let entry_span = self.tok().span;
            let entry_name = self.text().to_string();
            self.bump();
            // Optional constructor arguments: `RED(0xFF0000)`, incl. named `RED(rgb = 0xFF0000)`.
            let (args, arg_names) = if self.eat(TokenKind::LParen) {
                let (args, names, _) = self.parse_call_argument_list();
                self.expect(TokenKind::RParen, "')'");
                (args, names)
            } else {
                (Vec::new(), Vec::new())
            };
            // A per-entry class body (`RED { override fun m() = … }`) is an anonymous subclass.
            // Capture its method overrides and `val`/`var` properties; anything else bails cleanly.
            let mut body = Vec::new();
            let mut bprops = Vec::new();
            let mut init_order = Vec::new();
            if self.eat(TokenKind::LBrace) {
                let entry_nested_start = self.file.decls.len();
                self.skip_semicolons();
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    let bmods = self.parse_member_decl_prefix();
                    let entry_owner = format!("{name}.{entry_name}");
                    let nested_start = self.file.decls.len();
                    if self.parse_and_register_nested_classifier(&entry_owner, &bmods, true) {
                        if let Some(&declaration) = self.file.decls.get(nested_start) {
                            self.file
                                .enum_entry_nested_classifier_owners
                                .insert(declaration, entry_span);
                        }
                        // An `inner` classifier declared by an enum entry captures the entry value,
                        // whose source type is the enum—not a separately nameable entry-subclass
                        // type. Keep the JVM/source nesting path (`Enum.ENTRY.Inner`) while recording
                        // the semantic dispatch receiver (`Enum`) independently.
                        if bmods.iter().any(|modifier| modifier == "inner") {
                            if let Some(&declaration) = self.file.decls.get(nested_start) {
                                if let Decl::Class(nested) = self.file.decl_mut(declaration) {
                                    nested.inner_of = Some(name.to_string());
                                }
                            }
                        }
                    } else if self.at(TokenKind::KwFun) {
                        body.push(self.parse_fun(&bmods));
                    } else if self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar) {
                        let property = self.parse_member_property(
                            &bmods,
                            false,
                            bmods.iter().any(|m| m == "abstract"),
                        );
                        init_order.push(ClassInit::PropInit(bprops.len()));
                        bprops.push(property);
                    } else if self.at(TokenKind::Ident)
                        && self.keyword_text("init")
                        && self
                            .t
                            .get(self.i + 1)
                            .is_some_and(|token| token.kind == TokenKind::LBrace)
                    {
                        init_order.push(ClassInit::Block(self.parse_init_block()));
                    } else {
                        self.diags
                            .error(self.tok().span, "expected an enum-entry member declaration");
                        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                    }
                    self.skip_semicolons();
                }
                self.expect(TokenKind::RBrace, "'}'");
                // Classifiers are parser-hoisted out of an enum-entry body. Retain the
                // structural entry boundary for every outermost classifier created while
                // parsing that body, including anonymous objects in properties, methods,
                // and init blocks. Descendants remain owned by their containing classifier
                // through source containment; attaching all newly created declarations
                // directly to the entry would flatten anonymous/named nesting.
                let nested = self.file.decls[entry_nested_start..]
                    .iter()
                    .copied()
                    .filter(|declaration| matches!(self.file.decl(*declaration), Decl::Class(_)))
                    .collect::<Vec<_>>();
                for &declaration in &nested {
                    let Decl::Class(class) = self.file.decl(declaration) else {
                        continue;
                    };
                    let has_nested_owner = nested.iter().copied().any(|candidate| {
                        if candidate == declaration {
                            return false;
                        }
                        matches!(self.file.decl(candidate), Decl::Class(owner)
                                if owner.span.lo <= class.span.lo
                                    && class.span.hi <= owner.span.hi)
                    });
                    if !has_nested_owner {
                        self.file
                            .enum_entry_nested_classifier_owners
                            .entry(declaration)
                            .or_insert(entry_span);
                    }
                }
            }
            let entry_end = comment_binding::trailing_end(
                self.src,
                self.syntactic_ident_span(self.t[self.i.saturating_sub(1)])
                    .hi,
            );
            entries.push(AstEnumEntry {
                name: entry_name,
                span: entry_span,
                declaration_span: Span::new(entry_start, entry_end),
                decl_line: 0,
                annotations: entry_ann_names,
                annotation_args: entry_ann_args,
                args,
                arg_names,
                methods: body,
                props: bprops,
                init_order,
            });
            self.skip_semicolons();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            // After a trailing comma, a `;` still ends the entry list: `A, ; constructor(…)`
            // declares a member, not an entry `constructor`.
            let ends_entries = self.at(TokenKind::Semicolon);
            self.skip_semicolons();
            if ends_entries {
                break;
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_texts(source: &str) -> Vec<(String, String)> {
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        file.decls
            .iter()
            .filter_map(|&id| match file.decl(id) {
                Decl::Class(class) => Some(class),
                _ => None,
            })
            .flat_map(|class| class.enum_entries.iter())
            .map(|entry| {
                let range = entry.declaration_span;
                (
                    entry.name.clone(),
                    source[range.lo as usize..range.hi as usize].to_string(),
                )
            })
            .collect()
    }

    /// Every expectation below is the text of the entry's PSI element as kotlinc's parser builds it,
    /// read through the Analysis API's standalone session.
    fn assert_entries(source: &str, expected: &[(&str, &str)]) {
        let expected = expected
            .iter()
            .map(|(name, text)| (name.to_string(), text.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(entry_texts(source), expected);
    }

    #[test]
    fn range_covers_annotations_arguments_body_and_the_separator() {
        assert_entries(
            "enum class E(val v: Int) {\n    /** doc */\n    A(1),\n    @Deprecated(\"x\") B(2) { },\n    // c\n    C(3), `in`,\n    D;\n    fun f() = 1\n}\n",
            &[
                ("A", "/** doc */\n    A(1),"),
                ("B", "@Deprecated(\"x\") B(2) { },"),
                ("C", "// c\n    C(3),"),
                ("in", "`in`,"),
                ("D", "D;"),
            ],
        );
    }

    #[test]
    fn comments_lead_the_next_entry_from_the_line_after_the_previous_token() {
        assert_entries(
            "enum class E {\n    A, // trailing a\n    // own line b\n    B,\n\n    // after blank\n    C,\n    /* block */ D,\n    /** kdoc */\n\n    E1, // t\n    F // t2\n    ;\n}\n",
            &[
                ("A", "A, // trailing a"),
                ("B", "// own line b\n    B,"),
                ("C", "// after blank\n    C,"),
                ("D", "/* block */ D,"),
                ("E1", "/** kdoc */\n\n    E1, // t"),
                ("F", "F // t2\n    ;"),
            ],
        );
    }

    #[test]
    fn same_line_comments_stay_with_the_entry_before_them() {
        assert_entries(
            "enum class E { // c0\n    A /* x */, /* y */ B, // tb\n    /* z */ C /* w */ ; // after semi\n    fun f() = 1\n}\n",
            &[
                ("A", "A /* x */, /* y */"),
                ("B", "B, // tb"),
                ("C", "/* z */ C /* w */ ; // after semi"),
            ],
        );
    }

    #[test]
    fn an_entry_without_a_separator_keeps_its_trailing_comment() {
        assert_entries(
            "enum class E {\n    A, B // t\n}\n",
            &[("A", "A,"), ("B", "B // t")],
        );
        assert_entries(
            "enum class F {\n    A, B /* u */ }\n",
            &[("A", "A,"), ("B", "B /* u */")],
        );
    }

    #[test]
    fn escaped_entry_names_keep_their_backticks() {
        assert_entries(
            "enum class E { `in`, `is`(1), @Deprecated(\"x\") `fun` }",
            &[
                ("in", "`in`,"),
                ("is", "`is`(1),"),
                ("fun", "@Deprecated(\"x\") `fun`"),
            ],
        );
    }
}
