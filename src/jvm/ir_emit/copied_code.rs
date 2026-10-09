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

    /// A same-module expansion re-emits the inline body in the caller. When that expression was
    /// copied from an inline function whose private access is published, it calls the same
    /// accessors the published method does. A lambda the caller substituted into the expansion is
    /// not such a copy, so it keeps the caller's own access. Member references the copy interns
    /// are recorded as copied code, the same way [`Self::emitting`] records them.
    pub(super) fn with_copied_private_access(
        &mut self,
        expression: u32,
        emit: impl FnOnce(&mut Self),
    ) {
        let previous = self.export_private_calls;
        self.export_private_calls =
            self.method_exports_private_access || self.copy_exports_private_access(expression);
        self.emitting(expression, emit);
        self.export_private_calls = previous;
    }

    fn copy_exports_private_access(&self, expression: u32) -> bool {
        self.ir
            .inline_copy_provenance(expression)
            .is_some_and(|frames| {
                frames.iter().any(|frame| {
                    self.run
                        .static_accessor_plan
                        .borrow()
                        .exports_private_access(frame.function)
                })
            })
    }
}
