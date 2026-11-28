//! The source position of every function declaration, by [`FunId`].
//!
//! A function's lines belong to the function, not to tables a copy must remember one by one: they
//! live in a column parallel to [`IrFile::functions`], so copying a function copies its whole
//! position. A function built without a source declaration has none.

use super::{FunId, IrFile};

/// The source position of one function declaration. `None` is a function without that position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrFunctionPosition {
    /// The line the method body maps to: the `fun` declaration's line, or an expression body's own
    /// line. A function with this line is a source declaration whose body has a line.
    pub decl_line: Option<u32>,
    /// The line of the declaration itself, never rewritten by the body. A `$default` stub maps here;
    /// it differs from [`Self::decl_line`] exactly when an expression body starts on a later line.
    pub signature_line: Option<u32>,
    /// The line of a block body's closing `}`, where kotlinc maps a `Unit` function's implicit
    /// `return`.
    pub close_line: Option<u32>,
    /// The line the declaration ends on (a block's `}` or an expression body's last line), where
    /// kotlinc returns a generated forwarder of the declaration.
    pub end_line: Option<u32>,
    /// The source byte offset of the `fun` keyword, where a platform declaration clash is reported.
    /// A method without it is a bridge, a default stub or another representation, not a source
    /// declaration.
    pub signature_offset: Option<u32>,
}

impl IrFunctionPosition {
    /// The line the declaration's header stands on: its signature line, or the line of a declaration
    /// that records only one (an accessor's header line is its declaration line).
    pub fn header_line(self) -> Option<u32> {
        self.signature_line.or(self.decl_line)
    }
}

/// The positions of every function, by [`FunId`].
#[derive(Default)]
pub(crate) struct IrFunctionPositions {
    functions: Vec<IrFunctionPosition>,
}

impl IrFunctionPositions {
    fn get(&self, function: FunId) -> IrFunctionPosition {
        self.functions
            .get(function as usize)
            .copied()
            .unwrap_or_default()
    }

    fn get_mut(&mut self, function: FunId) -> &mut IrFunctionPosition {
        let index = function as usize;
        if self.functions.len() <= index {
            self.functions
                .resize(index + 1, IrFunctionPosition::default());
        }
        &mut self.functions[index]
    }
}

impl IrFile {
    /// The source position of `function`.
    pub fn function_position(&self, function: FunId) -> IrFunctionPosition {
        self.function_positions.get(function)
    }

    /// The source position of `function`, to record or change it.
    pub fn function_position_mut(&mut self, function: FunId) -> &mut IrFunctionPosition {
        self.function_positions.get_mut(function)
    }

    /// Give `target` the source position `source` has.
    pub(crate) fn copy_function_position(&mut self, source: FunId, target: FunId) {
        let position = self.function_position(source);
        *self.function_position_mut(target) = position;
    }
}

#[cfg(test)]
mod tests {
    use super::IrFunctionPosition;
    use crate::ir::IrFile;

    #[test]
    fn a_function_without_a_recorded_position_has_none() {
        let mut ir = IrFile::default();
        ir.function_position_mut(3).decl_line = Some(7);

        assert_eq!(ir.function_position(1), IrFunctionPosition::default());
        assert_eq!(ir.function_position(9), IrFunctionPosition::default());
        assert_eq!(ir.function_position(3).decl_line, Some(7));
    }

    #[test]
    fn a_copied_function_takes_its_source_position_whole() {
        let mut ir = IrFile::default();
        let source = IrFunctionPosition {
            decl_line: Some(4),
            signature_line: Some(3),
            close_line: Some(9),
            end_line: Some(9),
            signature_offset: Some(41),
        };
        *ir.function_position_mut(0) = source;
        ir.function_position_mut(1).end_line = Some(2);

        ir.copy_function_position(0, 1);

        assert_eq!(ir.function_position(1), source);
    }
}
