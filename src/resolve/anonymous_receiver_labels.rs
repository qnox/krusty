//! The label an anonymous object's body uses for an enclosing receiver it captures.
//!
//! The object's body is checked again after its construction site, and `this@label` there must
//! name the same receiver-tower rung it named at the construction. The capture carries the label
//! so that rung is re-entered once, under the spelling the body uses, instead of being added a
//! second time.

use super::*;

impl Checker<'_> {
    /// The label `receiver` answers to: an extension declaration's label, a receiver lambda's label
    /// (`outer` for `build outer@{ … }`, not its `this` binding), or a context receiver's name or
    /// label. A class receiver is reached through the enclosing-instance chain and carries none.
    pub(super) fn anonymous_capture_receiver_label(
        &self,
        scope: &CheckerScope<'_>,
        receiver: &ImplicitReceiver,
    ) -> Option<Box<str>> {
        if receiver.class_receiver {
            return None;
        }
        let identity = receiver.identity;
        receiver
            .extension_receiver
            .and_then(|declaration| {
                self.extension_receiver_labels
                    .iter()
                    .rev()
                    .find_map(|(index, candidate)| {
                        (*candidate == declaration).then(|| {
                            self.this_labels
                                .get(*index)
                                .map(|(label, _, _)| label.clone())
                        })
                    })
                    .flatten()
            })
            .or_else(|| scope.implicit_receiver_lambda_label(identity))
            .or_else(|| scope.implicit_receiver_context_name(identity))
            .or_else(|| scope.implicit_receiver_context_label(identity))
            .map(String::into_boxed_str)
    }
}
