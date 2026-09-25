//! The stdlib's scope functions, when their block is not a lambda written at the call site.
//!
//! `x.apply { … }` never reaches a backend: `apply` is `inline`, and the checked lowering splices
//! the block into the caller, which is what `inline` means. What DOES reach here is the other
//! shape — `Buildee<T>().apply(instructions)`, where the block is a function-typed parameter. There
//! is no body to splice, so the call survives as a call to a stdlib member the runtime does not
//! have, and 73 corpus cases declined on it. Which member it is, `src/native/intrinsics.rs` says:
//! spelling a provider's names is that module's job, not this one's.
//!
//! Each of the four is one line of rearrangement, and Kotlin's own signature says which:
//!
//! | call | the block is handed | the call yields |
//! |---|---|---|
//! | `T.apply(block: T.() -> Unit): T` | the receiver | the receiver |
//! | `T.also(block: (T) -> Unit): T` | the receiver | the receiver |
//! | `T.let(block: (T) -> R): R` | the receiver | the block's result |
//! | `T.run(block: T.() -> R): R` | the receiver | the block's result |
//!
//! The receiver is evaluated ONCE — it is the whole point of `let` that `x` is computed once —
//! and before the block, which is the order they are written in.

use super::super::super::intrinsics::{Builder, ScopeResult, TopLevelScope};
use super::*;

impl BodyLowering<'_, '_, '_> {
    /// Realize a scope function whose block is an ordinary function value, or `None` when this is
    /// not one of them.
    pub(super) fn scope_function(
        &mut self,
        owner: &str,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let yields = super::super::super::intrinsics::scope_function(owner, name)?;
        Some(self.call_with_receiver(yields, receiver, args, ret))
    }

    /// The same, for the two that take their subject as an ARGUMENT rather than as a receiver.
    ///
    /// `run { … }` invokes its block with nothing; `with(x) { … }` invokes it with `x`, which is
    /// the same call `x.run { … }` makes — the two differ only in where the subject is written.
    pub(super) fn top_level_scope_function(
        &mut self,
        owner: &str,
        name: &str,
        args: &[u32],
        params: &[Ty],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let scope = super::super::super::intrinsics::top_level_scope(owner, name, params)?;
        Some(match (scope, args) {
            (TopLevelScope::Block, [block]) => self.invoke_block(*block, &[], ret),
            (TopLevelScope::WithReceiver, [receiver, block]) => {
                self.call_with_receiver(ScopeResult::BlockResult, *receiver, &[*block], ret)
            }
            _ => Err("a receiverless scope function with an unexpected argument shape".to_string()),
        })
    }

    /// Evaluate a block value and invoke it.
    fn invoke_block(
        &mut self,
        block: u32,
        arguments: &[Value],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let function = self.reference(block)?;
        if self.terminated {
            return Ok(None);
        }
        self.invoke_value(function, arguments, ret)
    }

    fn call_with_receiver(
        &mut self,
        yields: ScopeResult,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let [block] = args else {
            return Err("a scope function with more than a block".to_string());
        };
        let value = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        let function = self.reference(*block)?;
        if self.terminated {
            return Ok(None);
        }
        let returned = self.invoke_value(function, &[value], ret)?;
        match yields {
            ScopeResult::Receiver => self.convert(value, Some(any()), ret),
            ScopeResult::BlockResult => Ok(returned),
        }
    }
}

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
            _ => return Err("a `build` function with an unexpected argument shape".to_string()),
        };
        // The capacity is written BEFORE the block, and is evaluated in that order.
        let capacity = match capacity {
            Some(expression) => match self.coerce(expression, Ty::Int)? {
                Some(value) => Some(value),
                None => return Err("a `build` function with a `Unit` capacity".to_string()),
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
