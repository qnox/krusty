//! kotlinc's redundant boxing elimination (`codegen/optimization/boxing/`): a value boxed only to
//! be unboxed again, compared, or kept in a local is never boxed. Inlining makes most of these —
//! a lambda's primitive parameter arrives through the `Object` its `invoke` takes — so the output
//! of an inlined call depends on this pass as much as on the inliner.

mod interpreter;
mod recognizers;
mod rewrite;
mod values;

#[cfg(test)]
mod tests;

pub(crate) use recognizers::{is_boxing, is_iterator_call, is_primitive_boxing_insn, ValueClasses};
pub(crate) use rewrite::eliminate;
pub(crate) use values::progression_iterator;

/// The value classes a class's code may box: internal name → the descriptor of the unboxed value
/// its `box-impl` takes (kotlinc's `unboxedTypeOfInlineClass`).
#[derive(Default)]
pub struct ValueClassDescriptors(crate::fx_hash::HashMap<String, String>);

impl ValueClassDescriptors {
    /// Descriptors keyed by the value class's internal name.
    pub fn new(descriptors: crate::fx_hash::HashMap<String, String>) -> Self {
        Self(descriptors)
    }
}

impl ValueClasses for ValueClassDescriptors {
    fn underlying_type(&self, internal_name: &str) -> Option<String> {
        self.0.get(internal_name).cloned()
    }
}
