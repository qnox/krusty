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
        .expect("the signature result must be evaluated before publication");
    Ok(recover(semantics, expression, graph, memo)?.unwrap_or((
        current,
        false,
        result_abbreviation(semantics, graph, expression, memo),
    )))
}

/// Recover one completely evaluated publication subtree. Ordinary signature evaluation may
/// deliberately recover a join from an untypeable operand without memoizing that operand. Such a
/// partial subtree carries no sound declaration-publication evidence: keep the already-evaluated
/// ordinary result transactionally instead of publishing evidence from only the surviving sides.
fn recover<S: SignatureSemantics>(
    semantics: &S,
    expression: SigExprId,
    graph: &SignatureGraph,
    memo: &HashMap<SigExprId, ResolvedTy>,
) -> Result<Option<(ResolvedTy, bool, Option<ResolvedTypeAbbreviation>)>, DiagnosticId> {
    let Some(&current) = memo.get(&expression) else {
        return Ok(None);
    };
    match graph.expr(expression) {
        Some(SigExpr::MutableLocalFlow { declared, .. }) => {
            let Some(&published) = memo.get(&declared) else {
                return Ok(None);
            };
            Ok(Some((
                published,
                true,
                result_abbreviation(semantics, graph, declared, memo),
            )))
        }
        Some(SigExpr::Sequence { result, .. }) | Some(SigExpr::ScopedReceiver { result, .. }) => {
            recover(semantics, result, graph, memo)
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
                let Some((ty, operand_changed, abbreviation)) =
                    recover(semantics, operand, graph, memo)?
                else {
                    return Ok(None);
                };
                publication.push(ty);
                abbreviations.push(abbreviation);
                changed |= operand_changed;
            }
            if !changed {
                return Ok(Some((
                    current,
                    false,
                    result_abbreviation(semantics, graph, expression, memo),
                )));
            }
            let scope = graph
                .scope(scope)
                .expect("a publication join retains its signature scope");
            let result = semantics.least_upper_bound(scope, origin, &publication)?;
            let abbreviation = publication
                .iter()
                .position(|operand| *operand == result)
                .and_then(|index| abbreviations[index].clone());
            Ok(Some((result, true, abbreviation)))
        }
        Some(SigExpr::Nullable(base)) => {
            let Some((published, changed, abbreviation)) = recover(semantics, base, graph, memo)?
            else {
                return Ok(None);
            };
            if changed {
                semantics
                    .make_nullable(published)
                    .map(|ty| Some((ty, true, abbreviation)))
            } else {
                Ok(Some((current, false, abbreviation)))
            }
        }
        Some(SigExpr::NonNullable(base)) => {
            let Some((published, changed, abbreviation)) = recover(semantics, base, graph, memo)?
            else {
                return Ok(None);
            };
            if changed {
                semantics
                    .make_non_nullable(published)
                    .map(|ty| Some((ty, true, abbreviation)))
            } else {
                Ok(Some((current, false, abbreviation)))
            }
        }
        _ => Ok(Some((
            current,
            false,
            result_abbreviation(semantics, graph, expression, memo),
        ))),
    }
}
