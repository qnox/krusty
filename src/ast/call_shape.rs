use super::{Expr, ExprId, File};

impl File {
    /// Function literals and callable references await a selected callable shape. Class literals
    /// are ordinary values; the parser records their syntax separately from backticked `class`.
    pub(crate) fn is_callable_syntax(&self, expression: ExprId) -> bool {
        matches!(self.expr(expression), Expr::Lambda { .. })
            || (matches!(self.expr(expression), Expr::CallableRef { .. })
                && !self.class_literal_references.contains(&expression.0))
    }
}

pub fn first_lambda_param_or_it(params: &[String]) -> String {
    params.first().cloned().unwrap_or_else(|| "it".to_string())
}

pub fn lambda_params_or_implicit(
    params: &[String],
    arity: usize,
    has_explicit_arrow: bool,
) -> Option<Vec<String>> {
    if !params.is_empty() {
        Some(params.to_vec())
    } else if arity == 1 && !has_explicit_arrow {
        Some(vec![first_lambda_param_or_it(params)])
    } else if arity == 0 {
        Some(Vec::new())
    } else {
        None
    }
}

/// The source receiver written on an ordinary or safe call. This is syntax ownership only;
/// resolution still selects the declaration and receiver type.
pub(crate) fn explicit_call_receiver(file: &File, expression: ExprId) -> Option<ExprId> {
    match file.expr(expression) {
        Expr::Call { callee, .. } => match file.expr(*callee) {
            Expr::Member { receiver, .. } => Some(*receiver),
            _ => None,
        },
        Expr::SafeCall { receiver, .. } => Some(*receiver),
        _ => None,
    }
}
