use super::*;

fn span() -> Span {
    Span::new(0, 0)
}

#[test]
fn binary_operator_location_does_not_widen_the_expression_arena_entry() {
    assert!(
        std::mem::size_of::<Expr>() <= 104,
        "Expr grew to {} bytes",
        std::mem::size_of::<Expr>()
    );
}

#[test]
fn expr_uses_name_stops_at_nested_lambdas() {
    let mut file = File::default();
    let outer = file.add_expr(Expr::Name("outer".to_string()), span());
    let lambda = file.add_expr(
        Expr::Lambda {
            params: Vec::new(),
            body: outer,
        },
        span(),
    );

    assert!(!file.expr_uses_name(lambda, "outer"));
    assert!(file.expr_uses_name_deep(lambda, "outer"));
}

#[test]
fn expr_uses_name_deep_respects_lambda_parameter_shadowing() {
    let mut file = File::default();
    let outer = file.add_expr(Expr::Name("outer".to_string()), span());
    let lambda = file.add_expr(
        Expr::Lambda {
            params: vec!["outer".to_string()],
            body: outer,
        },
        span(),
    );

    assert!(!file.expr_uses_name_deep(lambda, "outer"));
}

#[test]
fn expr_uses_name_counts_assignment_targets_and_values() {
    let mut file = File::default();
    let value = file.add_expr(Expr::Name("value".to_string()), span());
    let assign = file.add_stmt(
        Stmt::Assign {
            name: "target".to_string(),
            value,
        },
        span(),
    );
    let block = file.add_expr(
        Expr::Block {
            stmts: vec![assign],
            trailing: None,
        },
        span(),
    );

    assert!(file.expr_uses_name(block, "target"));
    assert!(file.expr_uses_name(block, "value"));
    assert!(!file.expr_uses_name(block, "missing"));
}

#[test]
fn const_string_value_renders_a_char_as_its_code_unit() {
    let mut file = File::default();
    let dollar = file.add_expr(Expr::CharLit(b'$' as u16), span());
    let surrogate = file.add_expr(Expr::CharLit(0xD800), span());

    assert_eq!(file.const_string_value(dollar), Some(KtString::from("$")));
    // A lone surrogate is a legal `Char` with no Unicode-scalar form. It must survive the fold
    // as its code unit rather than make the whole constant unrepresentable.
    let folded = file.const_string_value(surrogate).expect("lone surrogate");
    assert_eq!(folded.units().collect::<Vec<_>>(), vec![0xD800]);
    assert_eq!(folded.as_str(), None);
}

#[test]
fn const_string_value_joins_a_template_in_code_units() {
    // The two halves of U+1F600 arriving as separate `Char` parts must rejoin into the ordinary
    // two-unit string, not stay in the degraded form.
    let mut file = File::default();
    let high = file.add_expr(Expr::CharLit(0xD83D), span());
    let low = file.add_expr(Expr::CharLit(0xDE00), span());
    let template = file.add_expr(
        Expr::Template(vec![
            TemplatePart::Str(KtString::from("[")),
            TemplatePart::Expr(high),
            TemplatePart::Expr(low),
            TemplatePart::Str(KtString::from("]")),
        ]),
        span(),
    );

    assert_eq!(
        file.const_string_value(template),
        Some(KtString::from("[\u{1F600}]"))
    );
}
