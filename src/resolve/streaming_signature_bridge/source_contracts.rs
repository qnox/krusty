//! Pass-1 source-contract extraction and semantic publication.
//!
//! A source contract affects callers, so it is declaration data rather than an ordinary body. The
//! active parser unit is decoded immediately into a compact temporary payload. The signature
//! environment then binds its callee and condition types by stable declaration identity before the
//! payload enters `ResolvedModuleIndex`; no parser id, source range, or unresolved `TypeRef` crosses
//! into Pass 2.

use crate::ast::{Decl, Expr, File, FunBody, Stmt};

use super::ProductionSignatureSemantics;

#[derive(Clone, Debug)]
pub(crate) struct SourceContractCandidate {
    declaration: crate::fir::DeclarationId,
    source: crate::fir::SourceFileId,
    callee: Box<str>,
    shadowed_by_parameter: bool,
    contract: crate::contracts::Contract,
}

/// Decode contract-shaped first statements while the bounded Pass-1 parser unit is active.
/// Semantic classification is deliberately deferred: aliases, imports, module shadowing, and
/// provider identity belong to the normal signature resolver below, not to syntax recognition.
pub(crate) fn extract_source_contract_candidates(
    file: &File,
    active: &crate::fir::ActiveSourceHeaders<'_>,
) -> Vec<SourceContractCandidate> {
    file.decls
        .iter()
        .filter_map(|&parser_declaration| {
            let declaration = active.declaration(parser_declaration)?;
            let Decl::Fun(function) = file.decl(parser_declaration) else {
                return None;
            };
            let FunBody::Block(body) = function.body else {
                return None;
            };
            let Expr::Block { stmts, trailing } = file.expr(body) else {
                return None;
            };
            // Kotlin requires the contract declaration to be the function's first statement. A
            // single trailing expression is the same first statement in the parser's block form.
            let expression = stmts
                .first()
                .and_then(|statement| match file.stmt(*statement) {
                    Stmt::Expr(expression) => Some(*expression),
                    _ => None,
                })
                .or_else(|| stmts.is_empty().then_some(*trailing).flatten())?;
            let Expr::Call { callee, .. } = file.expr(expression) else {
                return None;
            };
            let Expr::Name(callee) = file.expr(*callee) else {
                return None;
            };
            let contract = crate::contracts::decode_source(
                file,
                expression,
                &function
                    .params
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect::<Vec<_>>(),
                &function.name,
                function.receiver.is_some(),
            )?;
            Some(SourceContractCandidate {
                declaration,
                source: active.source(),
                callee: callee.clone().into_boxed_str(),
                shadowed_by_parameter: function
                    .params
                    .iter()
                    .any(|parameter| parameter.name == *callee),
                contract,
            })
        })
        .collect()
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn resolve_source_contracts(
        &self,
        candidates: &[SourceContractCandidate],
    ) -> Result<
        Vec<(
            crate::fir::DeclarationId,
            crate::contracts::ResolvedContract,
        )>,
        Vec<crate::fir::DeclarationId>,
    > {
        let mut resolved = Vec::new();
        let mut failed = Vec::new();
        for candidate in candidates {
            if candidate.shadowed_by_parameter {
                continue;
            }
            let scope = crate::fir::SignatureScope {
                owner: candidate.declaration,
                source: candidate.source,
            };
            let intrinsic = self
                .with_resolver(scope, |resolver| {
                    let overloads = resolver.top_level_candidates(&candidate.callee);
                    let mut functions =
                        crate::libraries::FunctionSet { overloads }.into_top_level();
                    let first = functions.next()?;
                    (self
                        .table
                        .libraries
                        .is_erased_contract_callable(&first.callable)
                        && functions.all(|function| {
                            self.table
                                .libraries
                                .is_erased_contract_callable(&function.callable)
                        }))
                    .then_some(())
                })
                .is_ok();
            if !intrinsic {
                continue;
            }
            let contract = candidate.contract.with_resolved_types(&mut |reference| {
                self.with_signature_type_scope(scope, |lexical| {
                    self.signature_type_ref(scope, lexical, reference)
                })
                .ok()
                .flatten()
                .filter(|ty| !ty.mentions_pending() && !ty.mentions_error())
            });
            match crate::contracts::ResolvedContract::new(contract) {
                Ok(contract) => resolved.push((candidate.declaration, contract)),
                Err(error) => {
                    crate::trace_compiler!(
                        "signature",
                        "source contract {:?} is not publishable: {error:?}",
                        candidate.declaration,
                    );
                    failed.push(candidate.declaration);
                }
            }
        }
        if failed.is_empty() {
            Ok(resolved)
        } else {
            failed.sort_by_key(|declaration| declaration.raw());
            failed.dedup();
            Err(failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Decl;
    use crate::diag::{DiagSink, Span};
    use crate::source::SourceInput;

    #[test]
    fn active_header_binding_does_not_join_contracts_by_source_range() {
        let source = "import kotlin.contracts.contract\n\
                      fun accepts(value: Any): Boolean {\n\
                          contract { returns(true) implies (value is String) }\n\
                          return true\n\
                      }";
        let input = SourceInput::kotlin(source).with_file_stem("ContractIdentity");
        let mut diagnostics = DiagSink::new();
        let file = crate::frontend::parse_source_with_detected_features(source, &mut diagnostics);
        let mut builder = crate::fir::HeaderInventoryBuilder::default();
        let active = builder
            .add_source(0, &input, Some(&file))
            .expect("valid Kotlin source has active headers");
        let parser_declaration = file.decls[0];
        let stable = active
            .declaration(parser_declaration)
            .expect("the source function has a stable declaration");

        let mut reparsed =
            crate::frontend::parse_source_with_detected_features(source, &mut diagnostics);
        let Decl::Fun(function) = reparsed.decl_mut(parser_declaration) else {
            panic!("the first declaration must remain the source function")
        };
        function.span = Span::new(source.len() as u32 + 10, source.len() as u32 + 20);

        let candidates = extract_source_contract_candidates(&reparsed, &active);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].declaration, stable);
    }
}
