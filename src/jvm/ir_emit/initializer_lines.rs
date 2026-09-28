//! An initializer's source lines: the property stores and `init { … }` blocks a constructor or a
//! static initializer runs, each on its own line even where no instruction of its own sits there.

use super::*;

impl Emitter<'_> {
    /// kotlinc's `init { … }` lines: the block opens on its `init` line and closes on its `}`, each
    /// an entry of its own even with no instruction there (see [`CodeBuilder::mark_line_occupied`]).
    pub(super) fn mark_initializer_line(&self, block: u32, closing: bool, code: &mut CodeBuilder) {
        if !self.ir.initializer_blocks.contains(&block) {
            return;
        }
        let lines = match closing {
            true => &self.ir.expr_end_lines,
            false => &self.ir.expr_source_lines,
        };
        if let Some(&line) = lines.get(&block).filter(|&&line| line != 0) {
            code.mark_line_occupied(line);
        }
    }

    /// Emit a constructor's lowered initializer block, collecting the lines its table maps. A pure
    /// `SetField` block maps each store's start pc to its recorded line; a block with `init`
    /// statements is emitted as one block and contributes the lines its statements marked.
    pub(super) fn emit_constructor_init_body(
        &mut self,
        class: &crate::ir::IrClass,
        init_body: crate::ir::ExprId,
        code: &mut CodeBuilder,
        lines: &mut Vec<(u16, u32)>,
    ) {
        let Some(stores) =
            crate::jvm::constructor_debug::initializer_property_stores(self.ir, class, init_body)
        else {
            // A block with `init` statements marks its own lines as it goes; keep them.
            let first = code.line_marks().len();
            self.emit(init_body, code);
            lines.extend(
                code.line_marks()[first..]
                    .iter()
                    .map(|&(pc, l)| (pc, u32::from(l))),
            );
            return;
        };
        for store in stores {
            let pc = code.bytes.len() as u16;
            if let Some(line) = store.line {
                lines.push((pc, line));
            }
            self.emit(store.expression, code);
        }
    }
}
