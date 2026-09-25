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

pub(crate) use recognizers::ValueClasses;
pub(crate) use rewrite::eliminate;
