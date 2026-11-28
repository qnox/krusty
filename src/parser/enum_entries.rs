use super::*;

impl Parser<'_> {
    /// Parses the entry list of an enum body up to the `;` that starts its members.
    pub(super) fn parse_enum_entries(&mut self, name: &str) -> Vec<AstEnumEntry> {
        let mut entries: Vec<AstEnumEntry> = Vec::new();
        // The gap in front of an entry starts where the previous entry, with the comments that
        // trail it, ends, or after the token that opens the body.
        let mut gap_start = self
            .i
            .checked_sub(1)
            .map_or(0, |previous| self.t[previous].span.hi);
        loop {
            let entry_start = comment_binding::leading_start(
                self.src,
                gap_start,
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
            let explicit_call = self.at(TokenKind::LParen);
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
            gap_start = entry_end;
            entries.push(AstEnumEntry {
                name: entry_name,
                span: entry_span,
                declaration_span: Span::new(entry_start, entry_end),
                decl_line: 0,
                annotations: entry_ann_names,
                annotation_args: entry_ann_args,
                explicit_call,
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

    /// Each fixture is a source with, for every entry, the text of its PSI element as kotlinc
    /// builds it. `tools/krusty-analysis-api` checks the same files against kotlinc's parser.
    fn assert_matches_psi(source: &str, expected: &str) {
        let expected = expected
            .lines()
            .map(|line| {
                let (name, text) = line.split_once('\t').expect("a name and a text");
                (name.to_string(), unescape(text))
            })
            .collect::<Vec<_>>();
        assert_eq!(entry_texts(source), expected);
    }

    fn unescape(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(character) = chars.next() {
            if character != '\\' {
                out.push(character);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        out
    }

    macro_rules! psi_fixtures {
        ($($name:ident),+ $(,)?) => {$(
            #[test]
            fn $name() {
                assert_matches_psi(
                    include_str!(concat!("../../tests/psi_ranges/enum_entries/", stringify!($name), ".kt")),
                    include_str!(concat!("../../tests/psi_ranges/enum_entries/", stringify!($name), ".expected")),
                );
            }
        )+};
    }

    psi_fixtures!(
        separators_and_bodies,
        comments_around_entries,
        same_line_comments,
        last_entry_trailing_line_comment,
        last_entry_trailing_block_comment,
        escaped_names,
        comment_before_blank_line,
        kdoc_after_plain_comment,
        only_comments_after_last_blank_line,
        comma_then_semicolon,
        comment_adjacent_to_brace,
        kdoc_trailing_previous_entry,
    );
}
