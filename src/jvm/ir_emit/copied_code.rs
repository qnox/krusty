//! Emission of expressions an inline expansion copied from an inline function's own body.
//!
//! kotlinc copies a compiled inline body's instructions into the call site, so the member
//! references they name are copied code the class's own signature mapping never saw. Every
//! expression entry point runs through [`Emitter::emitting`], which tells the class writer whether
//! the expression it emits is such a copy.

use super::Emitter;

impl Emitter<'_> {
    /// Run `emit` for expression `e`, with the member references it interns recorded as copied
    /// exactly when `e` is a copy of an inline function's own body.
    pub(super) fn emitting<T>(&mut self, e: u32, emit: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.cw.set_copying(self.ir.is_inline_copy(e));
        let result = emit(self);
        self.cw.set_copying(outer);
        result
    }
}
