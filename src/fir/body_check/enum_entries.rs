//! Checked representation of enum-entry reads.
//!
//! Resolution supplies the exact entry declaration when it belongs to this module. A read from
//! code lexically owned by that declaration uses the already-recorded receiver/capture graph;
//! every other entry read remains a target-realized static value.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn enum_entry_kind(
        &mut self,
        expression: ExprId,
        entry: &crate::resolve::ResolvedEnumEntry,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let ordinary = || FirExprKind::EnumEntry {
            classifier: entry.classifier,
            ordinal: entry.ordinal,
            name: entry.name.clone().into_boxed_str(),
        };
        let Some(path) = entry
            .declaration
            .and_then(|declaration| self.enclosing_enum_entry_path(declaration))
        else {
            return Ok(ordinary());
        };

        let origin = self.expression_origin(expression)?;
        let ty = self.expression_type(expression)?;
        let receiver = if self.body.local_callable().is_some() {
            // A callable directly under the entry may execute only after the constructor publishes
            // the static field. A callable nested through an inner classifier is part of that
            // classifier's construction path and must capture its enclosing entry parameter.
            if path.is_empty() {
                return Ok(ordinary());
            }
            self.body
                .add_implicit_receiver_capture(FirImplicitReceiverCapture {
                    origin,
                    enclosing_depth: 0,
                    current: false,
                    depth: 0,
                    path: path.clone().into_boxed_slice(),
                    ty,
                });
            self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::CapturedImplicitReceiver {
                    enclosing_depth: 0,
                    current: false,
                    depth: 0,
                    path: path.into_boxed_slice(),
                },
            })
        } else if path.is_empty() {
            let depth = self.receiver_frame().dispatch_depth.ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::MissingStableCallTarget,
                )
            })?;
            self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::ImplicitReceiver {
                    current: depth == 0,
                    depth,
                },
            })
        } else {
            self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::EnclosingReceiver {
                    path: path.into_boxed_slice(),
                },
            })
        };
        Ok(FirExprKind::TypeOperation {
            operation: FirTypeOperation::Cast,
            operand: receiver,
            target: ty,
        })
    }

    /// Classifiers between this body and its exact enclosing enum-entry declaration, innermost
    /// first. An empty path means the body belongs directly to the entry.
    fn enclosing_enum_entry_path(&self, target: DeclarationId) -> Option<Vec<DeclarationId>> {
        let mut current = DeclarationId::from_raw(self.body.owner().raw());
        let mut path = Vec::new();
        loop {
            if current == target {
                return Some(path);
            }
            let anchor = self.index.declaration_anchor(current)?;
            if anchor.kind == crate::fir::DeclarationKind::EnumEntry {
                return None;
            }
            if anchor.kind == crate::fir::DeclarationKind::Classifier {
                path.push(current);
            }
            current = anchor.owner?;
        }
    }
}
