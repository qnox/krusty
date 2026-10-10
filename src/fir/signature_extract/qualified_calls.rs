//! Parser coordinates retained for qualified calls until semantic root binding.

use crate::ast::{Expr, ExprId, File};

use super::{ExpressionForm, SignatureConstraintExtractor};
use crate::fir::{DeferredValueSelection, OriginId, SigExpr, SigExprId, SignatureScopeId};

pub(super) struct QualifiedCallablePath {
    pub(super) root: String,
    pub(super) first_selector: String,
    pub(super) spelling: String,
}

fn parser_path(file: &File, expression: ExprId) -> Option<QualifiedCallablePath> {
    fn collect(file: &File, expression: ExprId, names: &mut Vec<String>) -> bool {
        match file.expr(expression) {
            Expr::Name(name) => names.push(name.clone()),
            Expr::Member { receiver, name } if collect(file, *receiver, names) => {
                names.push(name.clone())
            }
            _ => return false,
        }
        true
    }

    let mut names = Vec::new();
    (collect(file, expression, &mut names) && names.len() >= 2).then(|| QualifiedCallablePath {
        root: names[0].clone(),
        first_selector: names[1].clone(),
        spelling: names.join("."),
    })
}

pub(super) fn callable_path(
    extractor: &SignatureConstraintExtractor,
    file: &File,
    expression: ExprId,
) -> Option<QualifiedCallablePath> {
    let path = parser_path(file, expression)?;
    (!extractor
        .lexical_values
        .iter()
        .rev()
        .any(|values| values.contains_key(path.root.as_str())))
    .then_some(path)
}

pub(super) fn spelling(
    extractor: &SignatureConstraintExtractor,
    file: &File,
    expression: ExprId,
) -> Option<String> {
    callable_path(extractor, file, expression).map(|path| path.spelling)
}

pub(super) fn value(
    extractor: &mut SignatureConstraintExtractor,
    file: &File,
    expression: ExprId,
    scope: SignatureScopeId,
    origin: OriginId,
) -> Option<SigExprId> {
    let spelling = spelling(extractor, file, expression)?;
    let spelling = extractor.graph.intern_name(&spelling);
    let selection = extractor.graph.add_value_selection(DeferredValueSelection {
        scope,
        spelling,
        origin,
        expected: None,
    });
    Some(extractor.graph.add_expr(SigExpr::Value(selection)))
}

/// Rebuild a qualifier as value/member nodes without deciding that its root is a value. The full
/// namespace spelling lives in parallel; evaluation binds the root and consumes only one shape.
pub(super) fn receiver_expression(
    extractor: &mut SignatureConstraintExtractor,
    file: &File,
    expression: ExprId,
    scope: SignatureScopeId,
    origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
) -> Result<SigExprId, ExpressionForm> {
    match file.expr(expression) {
        Expr::Name(_) => extractor.consumed_expression(file, expression, scope, origin),
        Expr::Member { receiver, name } => {
            let receiver = receiver_expression(extractor, file, *receiver, scope, origin)?;
            let fallback = file
                .expr_span(expression)
                .map(&mut *origin)
                .expect("a qualified call receiver must retain its source span");
            let selector_origin = SignatureConstraintExtractor::member_name_origin(
                file, expression, name, origin, fallback,
            );
            Ok(extractor.member(receiver, name, scope, selector_origin))
        }
        _ => unreachable!("a qualified callable path contains only names and members"),
    }
}
