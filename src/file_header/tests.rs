use super::*;
use crate::diag::DiagSink;

#[test]
fn syntax_and_streaming_scans_share_header_spans_and_paths() {
    let source = "@file:Suppress(\"unused\", value = 1 < 2)\npackage demo import q.First as Renamed import r.*\nfun import(value: Int) = value\n";
    let mut diagnostics = DiagSink::new();
    let tokens = crate::lexer::lex(source, &mut diagnostics);
    let header = from_tokens(source, &tokens);
    assert_eq!(header.package.as_deref(), Some("demo"));
    assert_eq!(
        header
            .imports
            .iter()
            .map(|item| (
                item.path.as_str(),
                item.wildcard,
                item.imported_name.as_deref()
            ))
            .collect::<Vec<_>>(),
        [("q.First", false, Some("Renamed")), ("r", true, None)]
    );
    let names = crate::lexer::lex_name_tokens(source, &mut diagnostics);
    assert_eq!(
        names
            .iter()
            .filter(|item| item.kind == crate::lexer::NameTokenKind::Import)
            .map(|item| item.span.lo)
            .collect::<Vec<_>>(),
        header.import_starts
    );
    let grammar_tokens = tokens
        .iter()
        .filter(|item| item.kind != K::Newline)
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        from_tokens(source, &grammar_tokens).import_starts,
        header.import_starts
    );
    // The complete syntax parser publishes the same inventory, not a second line-based scan.
    let file = crate::parser::parse(source, &tokens, &mut diagnostics);
    assert_eq!(
        file.import_paths
            .iter()
            .map(|item| (
                item.path(),
                item.wildcard,
                item.imported_name().map(str::to_owned)
            ))
            .collect::<Vec<_>>(),
        [
            ("q.First".to_owned(), false, Some("Renamed".to_owned())),
            ("r".to_owned(), true, None)
        ]
    );
    assert_eq!(
        diagnostics.diags.len(),
        0,
        "{}",
        diagnostics.render("test.kt", source)
    );
}

#[test]
fn ordinary_script_calls_and_backtick_names_never_open_import_lists() {
    for source in [
        "import(1)\nimport(2)\n",
        "`import`(1)\n",
        "fun import(value: Int) = value\nimport(1)\n",
    ] {
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        assert!(from_tokens(source, &tokens).imports.is_empty());
        assert_eq!(
            crate::lexer::lex_name_tokens(source, &mut diagnostics)
                .iter()
                .filter(|item| item.kind == crate::lexer::NameTokenKind::Import)
                .count(),
            0
        );
        let file = crate::parser::parse_script_with_features(
            source,
            &tokens,
            &mut diagnostics,
            &crate::features::LangFeatures::default(),
            "test",
            &crate::script_definitions::ScriptDefinition::standard(),
        );
        assert!(file.import_paths.is_empty());
        assert_eq!(
            diagnostics.diags.len(),
            0,
            "{}",
            diagnostics.render("test.kts", source)
        );
    }
}

#[test]
fn prefix_lookahead_is_replayed_before_the_remaining_stream() {
    let source = "import q.First\nfun import() { val later = 1 }";
    let mut diagnostics = DiagSink::new();
    let tokens = crate::lexer::lex(source, &mut diagnostics);
    let mut index = 0;
    let header = read(source, || {
        let token = tokens[index];
        index += 1;
        token
    });
    assert!(index < tokens.len());
    let replayed = header
        .prefix
        .into_iter()
        .chain(tokens[index..].iter().copied())
        .map(|token| (token.kind, token.span))
        .collect::<Vec<_>>();
    assert_eq!(
        replayed,
        tokens
            .iter()
            .map(|token| (token.kind, token.span))
            .collect::<Vec<_>>()
    );
    assert_eq!(diagnostics.diags.len(), 0);
}
