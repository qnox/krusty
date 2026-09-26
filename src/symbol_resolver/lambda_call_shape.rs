//! Contextual lambda facts carried by one selected callable.

use crate::types::Ty;

/// The argument-facing lambda shape of one selected call candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LambdaCallShape {
    /// Exact identities of the selected overload's generic formals. A postponed lambda frame may
    /// collect and suppress constraints only for these variables; symbolic types owned by an
    /// enclosing declaration remain fixed expectations.
    pub generic_formals: Vec<String>,
    /// Selected declaration parameters in source-argument order. This preserves the ordinary
    /// argument constraints that must be published before a later lambda is contextually checked,
    /// even when the selected callable comes from a provider rather than the source module.
    pub argument_parameters: Vec<Ty>,
    pub param_types: Option<Vec<Vec<Ty>>>,
    /// The selected callable parameter in source-argument order. Lambda checking consumes the
    /// decomposed inputs above; callable-reference adaptation needs the complete function type,
    /// including its return type, receiver bit, and still-unbound generic variables.
    pub expected_types: Option<Vec<Option<Ty>>>,
    /// Complete callable expectations whose result is fixed strongly enough to contextualize a
    /// lambda literal. Callable references may use the symbolic [`Self::expected_types`] above to
    /// select by input shape while contributing a result constraint; a lambda body must not be
    /// coerced to a widenable receiver lower bound before overload inference finishes.
    pub fixed_expected_types: Option<Vec<Option<Ty>>>,
    pub receivers: Option<Vec<Option<Ty>>>,
    pub context_counts: Option<Vec<usize>>,
    /// Per source argument, whether the selected parameter requires a materialized capture.
    /// `None` at either level means the declaration did not publish the parameter modifier; an
    /// inline permission must never be inferred from that missing fact.
    pub boxes_captures: Option<Vec<Option<bool>>>,
    /// The selected callable permits its non-materialized lambda arguments to be spliced.
    pub inline: bool,
}

impl LambdaCallShape {
    /// Whether the selected parameter of source argument `argument` inlines a lambda into the
    /// caller's frame: the callable is inline and the parameter is neither `crossinline` nor
    /// `noinline`.
    pub fn inlines_argument(&self, argument: usize) -> Option<bool> {
        if !self.inline {
            return Some(false);
        }
        self.boxes_captures
            .as_ref()?
            .get(argument)
            .copied()
            .flatten()
            .map(|boxes| !boxes)
    }
}
