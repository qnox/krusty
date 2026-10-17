//! Declaration-owned result recovery from an evaluated compact signature graph.

use super::{
    result_abbreviation, DiagnosticId, HashMap, ResolvedTy, ResolvedTypeAbbreviation, SigExpr,
    SigExprId, SignatureGraph, SignatureSemantics,
};

/// Recover the declaration-owned type of a mutable local without changing ordinary flow
/// evaluation. The compact graph retains the exact initializer identity; joins combine those
/// checked declaration values through the same semantic LUB operation as expressions.
pub(super) fn result<S: SignatureSemantics>(
    semantics: &S,
    expression: SigExprId,
    graph: &SignatureGraph,
    memo: &HashMap<SigExprId, ResolvedTy>,
) -> Result<(ResolvedTy, bool, Option<ResolvedTypeAbbreviation>), DiagnosticId> {
    let current = *memo
        .get(&expression)
        .expect("publication follows an already-evaluated signature expression");
    match graph.expr(expression) {
        Some(SigExpr::MutableLocalFlow { declared, .. }) => {
            let published = *memo
                .get(&declared)
                .expect("mutable-local evaluation records its declared value");
            Ok((
                published,
                true,
                result_abbreviation(semantics, graph, declared, memo),
            ))
        }
        Some(SigExpr::Sequence { result, .. }) | Some(SigExpr::ScopedReceiver { result, .. }) => {
            self::result(semantics, result, graph, memo)
        }
        Some(SigExpr::Join {
            operands,
            scope,
            origin,
        }) => {
            let operands = graph.operands(operands);
            let mut publication = Vec::with_capacity(operands.len());
            let mut abbreviations = Vec::with_capacity(operands.len());
            let mut changed = false;
            for &operand in operands {
                let (ty, operand_changed, abbreviation) =
                    self::result(semantics, operand, graph, memo)?;
                publication.push(ty);
                abbreviations.push(abbreviation);
                changed |= operand_changed;
            }
            if !changed {
                return Ok((
                    current,
                    false,
                    result_abbreviation(semantics, graph, expression, memo),
                ));
            }
            let scope = graph
                .scope(scope)
                .expect("a publication join retains its signature scope");
            let result = semantics.least_upper_bound(scope, origin, &publication)?;
            let abbreviation = publication
                .iter()
                .position(|operand| *operand == result)
                .and_then(|index| abbreviations[index].clone());
            Ok((result, true, abbreviation))
        }
        Some(SigExpr::Nullable(base)) => {
            let (published, changed, abbreviation) = self::result(semantics, base, graph, memo)?;
            if changed {
                semantics
                    .make_nullable(published)
                    .map(|ty| (ty, true, abbreviation))
            } else {
                Ok((current, false, abbreviation))
            }
        }
        Some(SigExpr::NonNullable(base)) => {
            let (published, changed, abbreviation) = self::result(semantics, base, graph, memo)?;
            if changed {
                semantics
                    .make_non_nullable(published)
                    .map(|ty| (ty, true, abbreviation))
            } else {
                Ok((current, false, abbreviation))
            }
        }
        _ => Ok((
            current,
            false,
            result_abbreviation(semantics, graph, expression, memo),
        )),
    }
}
