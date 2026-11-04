//! An initializer's source lines: the property stores and `init { … }` blocks a constructor or a
//! static initializer runs, each on its own line even where no instruction of its own sits there.

use super::*;

#[derive(Default)]
pub(super) struct ConstructorInitializerLines {
    entries: Vec<(u16, u32)>,
    restored: Vec<u16>,
}

impl ConstructorInitializerLines {
    pub(super) fn push(&mut self, entry: (u16, u32)) {
        self.entries.push(entry);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn record_restored(&mut self, code: &CodeBuilder, marks_before: usize) {
        record_restored_initializer_lines(
            code,
            marks_before,
            &mut self.entries,
            &mut self.restored,
        );
    }

    /// Build the exact constructor table, retaining an adjacent equal line only when the emitter
    /// identified that entry as an inlined line or its caller-line restoration.
    pub(super) fn method_entries(
        &self,
        start: (u16, u32),
        return_entry: (u16, u32),
    ) -> Vec<(u16, u32)> {
        let mut entries = vec![start];
        if !self.entries.is_empty() {
            entries.extend(self.entries.iter().copied());
            entries.push(return_entry);
        }
        entries
            .dedup_by(|(pc, line), (_, previous)| line == previous && !self.restored.contains(pc));
        entries
    }
}

impl Emitter<'_> {
    /// Whether this exact semantic initializer block is being emitted as class initialization.
    pub(super) fn renders_initializer_boundary(&self, block: u32) -> bool {
        self.render_initializer_boundaries && self.ir.initializer_blocks.contains(&block)
    }

    /// kotlinc's `init { … }` lines: the block opens on its `init` line and closes on its `}`, each
    /// an entry of its own even with no instruction there (see [`CodeBuilder::mark_line_occupied`]).
    pub(super) fn mark_initializer_line(&self, block: u32, closing: bool, code: &mut CodeBuilder) {
        if !self.renders_initializer_boundary(block) {
            return;
        }
        let line = match closing {
            true => self.ir.end_line(block),
            false => self.ir.source_line(block),
        };
        if let Some(line) = line.filter(|&line| line != 0) {
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
        lines: &mut ConstructorInitializerLines,
    ) {
        let Some(stores) =
            crate::jvm::constructor_debug::initializer_property_stores(self.ir, class, init_body)
        else {
            // A block with `init` statements marks its own lines as it goes; keep them.
            let first = code.line_marks().len();
            self.emit(init_body, code);
            lines.entries.extend(
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

/// The line entries a constructor's initializer statements recorded since `marks_before`, and the
/// offsets among them where a store writes its line again after an inlined value. kotlinc keeps
/// those even right after an entry of the same line.
pub(super) fn record_initializer_lines(
    code: &CodeBuilder,
    marks_before: usize,
    (lines, restored): (&mut Vec<(u16, u32)>, &mut Vec<u16>),
) {
    let recorded = &code.line_marks()[marks_before..];
    lines.extend(recorded.iter().map(|&(pc, line)| (pc, u32::from(line))));
    record_restored_initializer_lines(code, marks_before, lines, restored);
}

/// Keep the exact initializer marks whose producer identified them as an inlined line or the
/// caller-line restoration after one. Constructor debug-table curation must not collapse those
/// marks merely because the preceding entry names the same source line.
fn record_restored_initializer_lines(
    code: &CodeBuilder,
    marks_before: usize,
    lines: &mut Vec<(u16, u32)>,
    restored: &mut Vec<u16>,
) {
    let recorded = &code.line_marks()[marks_before..];
    restored.extend(
        code.inlined_line_marks()
            .filter(|mark| recorded.contains(mark))
            .map(|(pc, line)| {
                let entry = (pc, u32::from(line));
                if !lines.contains(&entry) {
                    lines.push(entry);
                }
                pc
            }),
    );
    lines.sort_by_key(|&(pc, _)| pc);
}
