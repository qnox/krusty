use super::*;

impl Parser<'_> {
    /// Whether the current token is `import` in a position where the grammar has an import list.
    /// `import` is a soft keyword: everywhere else it is an ordinary name.
    pub(super) fn at_import_directive(&self) -> bool {
        self.file_import_starts
            .binary_search(&self.tok().span.lo)
            .is_ok()
    }

    /// Parse one `import` directive, recording its path (and, for a single-name import, the
    /// classifier reference it names).
    pub(super) fn parse_import_directive(&mut self) {
        self.bump(); // 'import'
        let import_span = self.tok().span;
        let segments = self.parse_qualified_name_segments();
        let fq = segments
            .iter()
            .map(|(segment, _)| segment.as_str())
            .collect::<Vec<_>>()
            .join(".");
        // `import a.b.*` — the segment scan consumes the trailing `.` (it only keeps a
        // segment when an `Ident` follows), leaving us at `*`. Recover the wildcard so it is
        // recorded as `a.b.*` (the form the import-level builder recognizes).
        let mut wildcard = false;
        if self.at(TokenKind::Star) {
            self.bump(); // '*'
            wildcard = true;
        }
        let mut alias = None;
        if self.at(TokenKind::Ident) && self.keyword_text("as") {
            self.bump(); // 'as'
            if self.at(TokenKind::Ident) {
                alias = Some(self.text().to_string());
                self.bump();
            }
        }
        if !fq.is_empty() {
            if !wildcard {
                let mut reference = simple_type_ref(&fq, import_span);
                reference.flags = reference.flags.with_import(true);
                self.file.detached_type_refs.push(reference);
            }
            self.file.import_paths.push(crate::ast::ImportPath {
                segments,
                wildcard,
                alias,
            });
        }
    }
}
