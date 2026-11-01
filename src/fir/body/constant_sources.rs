//! Why a checked `FirExprKind::Constant` holds a value that source did not write as a literal.
//! Kotlin metadata's constant-initializer rule accepts only a literal as source wrote it.

use super::FirBody;
use crate::fir::FirExprId;

/// Why a `FirExprKind::Constant` that source did not write as a literal holds a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirConstantSource {
    /// The selected value of a `const val` or a Java constant field.
    DeclarationRead,
    /// A call the checker evaluated over a constant receiver (`trimIndent`, `trimMargin`); kotlinc
    /// folds it only after its frontend, so it is not a constant expression to the frontend.
    FoldedCall,
}

impl FirBody {
    pub(crate) fn record_constant_source(
        &mut self,
        expression: FirExprId,
        source: FirConstantSource,
    ) {
        self.constant_sources.insert(expression, source);
    }

    /// Where a constant that source did not write as a literal came from; `None` for a literal.
    pub fn constant_source(&self, expression: FirExprId) -> Option<FirConstantSource> {
        self.constant_sources.get(&expression).copied()
    }

    /// Whether this constant is a read of a constant declaration rather than a literal.
    pub fn is_constant_read(&self, expression: FirExprId) -> bool {
        self.constant_source(expression) == Some(FirConstantSource::DeclarationRead)
    }
}
