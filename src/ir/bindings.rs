//! Source-binding facts retained by backend-neutral IR.

/// Whether a binding read may observe a later source assignment.
///
/// Parameters, `val` locals, destructuring `val`s, loop variables, and catch parameters are stable;
/// a source `var` is mutable even when a backend keeps it in an ordinary local.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrBindingStability {
    Stable,
    Mutable,
}
