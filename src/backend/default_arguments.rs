//! Backend-boundary realization of calls that leave arguments to their defaults, for targets that
//! have no default-argument calling convention of their own.
//!
//! `fun greet(name: String, greeting: String = "hi")` called as `greet("k")` is the same function
//! with the missing argument computed by the callee: a default may read an earlier parameter
//! (`fun f(a: Int, b: Int = a + 1)`), so it runs in the declaration's frame. Each declaration with
//! defaults gets one entry, an ordinary function of its file taking `[receiver?] + parameters`
//! and one `Int` mask word per 32 parameters ([`entries`]); each call that omits arguments calls it
//! with placeholders and constant masks ([`call_sites`]). A top-level entry is published to the
//! other files of the module under the identity of the declaration that provides its defaults.
//!
//! Common lowering has already selected every callee and mapped every argument; this pass only
//! changes how an omission is carried. A target runs it after its other common realizations, and
//! realizes the [`crate::ir::IrExpr::AbsentArgument`] placeholders and the
//! [`crate::ir::Callee::ModuleDefaultEntry`] identities it leaves. The JVM keeps kotlinc's `$default`
//! methods and does not run it.

mod call_sites;
mod entries;
mod masks;
#[cfg(test)]
mod tests;

pub(crate) use entries::{published_entry, published_identities};
pub(crate) use masks::{mask_words, omission_masks};

use super::CheckedBackendCallables;
use crate::fx_hash::HashSet;
use crate::ir::IrFile;

/// Build the default-argument entries of `ir`'s declarations and rewrite every emitted call that
/// leaves arguments out to call one. `callables` are the facts frozen for `ir` before this
/// lowering; each expression an entry copies from a default gets the facts of its original.
pub(crate) fn realize(ir: &mut IrFile, callables: &mut CheckedBackendCallables) {
    let mut entries = entries::Entries::default();
    entries.publish(ir);
    entries.take_new_bodies();
    // Every emitted body, the entries' own included: a default may itself call with omissions.
    let mut pending = crate::ir::emitted_body_roots(ir);
    let mut seen = HashSet::default();
    while let Some(root) = pending.pop() {
        for expression in crate::ir::value_namespace_expressions(ir, root) {
            if seen.insert(expression) {
                call_sites::rewrite(ir, &mut entries, expression);
            }
        }
        pending.extend(entries.take_new_bodies());
    }
    callables.extend_to_clones(entries.clones());
}
