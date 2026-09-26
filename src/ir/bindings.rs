//! Source-binding facts retained by backend-neutral IR.

/// Whether a binding read may observe a later source assignment.
///
/// Parameters, `val` locals, destructuring `val`s, loop variables, and catch parameters are stable;
/// a source `var` is mutable even when a backend keeps it in an ordinary local.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrBindingStability {
    Stable,
    Mutable,
}

impl super::IrFile {
    /// The first value slot no function parameter, declaration, read, write, catch parameter or
    /// checked loop variable in this file uses. A backend pass that introduces a temporary takes
    /// its slot from here, so it never aliases a source binding.
    pub fn next_value_slot(&self) -> u32 {
        use super::{IrCheckedOperation, IrExpr};
        let parameter_slots = self
            .functions
            .iter()
            .map(|function| {
                function.params.len() as u32
                    + u32::from(function.dispatch_receiver.is_some() && !function.is_static)
            })
            .max()
            .unwrap_or(0);
        self.exprs
            .iter()
            .fold(parameter_slots, |highest, expression| {
                let index = match expression {
                    IrExpr::GetValue(index)
                    | IrExpr::SetValue { var: index, .. }
                    | IrExpr::Variable { index, .. }
                    | IrExpr::Checked(IrCheckedOperation::RangeLoop {
                        variable: index, ..
                    }) => Some(*index),
                    IrExpr::Try { catches, .. } => catches.iter().map(|catch| catch.var).max(),
                    _ => None,
                };
                index.map_or(highest, |index| {
                    highest.max(
                        index
                            .checked_add(1)
                            .expect("common-IR value slot exceeds u32"),
                    )
                })
            })
    }
}
