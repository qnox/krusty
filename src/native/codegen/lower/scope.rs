//! Native realization of stdlib builder bodies that do not yet have a decoded inline-body plan.
//!
//! Ordinary scope functions are absent here: their exact selected declaration publishes an
//! `InlineBodyPlan`, and common lowering expands that plan for both literal lambdas and ordinary
//! function values before any backend sees the call.

use super::super::super::intrinsics::Builder;
use super::*;

impl BodyLowering<'_, '_, '_> {
    /// `buildString { … }` and `buildList { … }`: a fresh subject, the block, then the subject.
    ///
    /// The same rearrangement the scope functions above get, with one difference — the subject is
    /// MADE here rather than written by the caller. Kotlin declares both `inline`, so a provider
    /// holding the body splices them; a klib publishes none and the call arrives whole.
    ///
    /// `buildString` answers the builder's `toString()`, which is a COPY: the builder is not the
    /// string, and a program that kept a reference to it through a capture would otherwise see the
    /// answer change under it. `buildList` answers the list it filled, because that is what
    /// Kotlin's own does — the read-only type is a static claim, not a runtime one.
    pub(super) fn builder_scope_function(
        &mut self,
        builder: Builder,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let (capacity, block) = match args {
            [block] => (None, *block),
            [capacity, block] => (Some(*capacity), *block),
            _ => return Err("a `build` function with an unexpected argument shape".into()),
        };
        // The capacity is written BEFORE the block, and is evaluated in that order.
        let capacity = match capacity {
            Some(expression) => match self.coerce(expression, Ty::Int)? {
                Some(value) => Some(value),
                None => return Err("a `build` function with a `Unit` capacity".into()),
            },
            None => None,
        };
        if self.terminated {
            return Ok(None);
        }
        let subject = match (builder, capacity) {
            (Builder::Text, None) => self.runtime_call("kt_string_builder_new", &[], any(), &[])?,
            (Builder::Text, Some(capacity)) => self.runtime_call(
                "kt_string_builder_with_capacity",
                &[Ty::Int],
                any(),
                &[capacity],
            )?,
            (Builder::List, None) => self.runtime_call("kt_mutable_list_new", &[], any(), &[])?,
            (Builder::List, Some(capacity)) => self.runtime_call(
                "kt_mutable_list_with_capacity",
                &[Ty::Int],
                any(),
                &[capacity],
            )?,
        };
        let Some(subject) = subject else {
            return Ok(None);
        };
        let function = self.reference(block)?;
        if self.terminated {
            return Ok(None);
        }
        self.invoke_value(function, &[subject], Ty::Unit)?;
        if self.terminated {
            return Ok(None);
        }
        match builder {
            // The builder's own `toString`, reached the way a lone `"$sb"` reaches it.
            Builder::Text => self.runtime_call("kt_to_string", &[any()], Ty::String, &[subject]),
            Builder::List => Ok(Some(subject)),
        }
    }
}
