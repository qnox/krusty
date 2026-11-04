//! What a checked expression's source form was where checking normalized it away. Kotlin
//! metadata's constant-initializer rule is kotlinc's frontend constant checker, which reads the
//! form source wrote: a literal, an operator, or an explicit call.

use super::FirBody;
use crate::fir::FirExprId;

/// The source form of a checked expression whose FIR shape does not show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirSourceForm {
    /// A `FirExprKind::Constant` holding the selected value of a `const val` or a Java constant
    /// field.
    DeclarationRead,
    /// A `FirExprKind::Constant` holding a call the checker evaluated over a constant receiver
    /// (`trimIndent`, `trimMargin`); kotlinc folds it only after its frontend, so it is not a
    /// constant expression to the frontend.
    FoldedCall,
    /// An operation source wrote as an explicit call (`"a".plus("b")`, `1.unaryPlus()`) that checks
    /// to the same FIR as its operator form. kotlinc's parser merges a `+` of string literals
    /// and reads `+1` as a literal; it does neither for the call.
    ExplicitCall,
    /// A number conversion source wrote as a property read (`'a'.code`). The checker accepts a
    /// conversion only as a `toInt()`-style call.
    PropertyRead,
}

impl FirBody {
    pub(crate) fn record_source_form(&mut self, expression: FirExprId, form: FirSourceForm) {
        self.source_forms.insert(expression, form);
    }

    /// The source form of `expression` where its FIR shape does not show it; `None` for a literal
    /// or an operation in its own syntax.
    pub fn source_form(&self, expression: FirExprId) -> Option<FirSourceForm> {
        self.source_forms.get(&expression).copied()
    }

    /// Whether this constant is a read of a constant declaration rather than a literal.
    pub fn is_constant_read(&self, expression: FirExprId) -> bool {
        self.source_form(expression) == Some(FirSourceForm::DeclarationRead)
    }
}
