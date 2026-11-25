use super::*;

impl Parser<'_> {
    /// Parses the entry list of an enum body up to the `;` that starts its members.
    pub(super) fn parse_enum_entries(&mut self, name: &str) -> Vec<AstEnumEntry> {
        let mut entries: Vec<AstEnumEntry> = Vec::new();
        loop {
            let entry_start = self.syntactic_ident_span(self.tok()).lo;
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
            let entry_end = self
                .syntactic_ident_span(self.t[self.i.saturating_sub(1)])
                .hi;
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

    #[test]
    fn enum_entry_range_covers_annotations_arguments_and_body() {
        let source = "enum class E(val v: Int) {\n    A(1),\n    @Deprecated(\"x\") B(2) { override fun toString() = \"b\" },\n    C(3);\n}";
        assert_eq!(
            entry_texts(source),
            [
                ("A".to_string(), "A(1)".to_string()),
                (
                    "B".to_string(),
                    "@Deprecated(\"x\") B(2) { override fun toString() = \"b\" }".to_string()
                ),
                ("C".to_string(), "C(3)".to_string()),
            ]
        );
    }

    #[test]
    fn bare_enum_entry_range_is_its_name() {
        assert_eq!(
            entry_texts("enum class E { A, B }"),
            [
                ("A".to_string(), "A".to_string()),
                ("B".to_string(), "B".to_string())
            ]
        );
    }

    #[test]
    fn escaped_enum_entry_names_keep_their_backticks() {
        assert_eq!(
            entry_texts("enum class E { `in`, `is`(1), @Deprecated(\"x\") `fun` }"),
            [
                ("in".to_string(), "`in`".to_string()),
                ("is".to_string(), "`is`(1)".to_string()),
                ("fun".to_string(), "@Deprecated(\"x\") `fun`".to_string()),
            ]
        );
    }
}
