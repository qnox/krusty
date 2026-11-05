//! JVM realization of the `-Xno-call-assertions` and `-Xno-receiver-assertions` policies.
//!
//! Checked FIR and common IR keep every implicit not-null cast on a narrowed platform value, with
//! the site that consumes the value. Whether a cast is emitted is a JVM backend option: the
//! policies remove the casts they select here, before emission. A source `x!!` is never selected.

use crate::ir::{IrExpr, IrFile, NullCheck};

/// Which implicit not-null casts the command line asks to omit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OmittedNullAssertions {
    /// `-Xno-call-assertions`: every implicit cast, whatever consumes the value.
    Implicit,
    /// `-Xno-receiver-assertions`: the casts of values that reach an extension receiver.
    ExtensionReceivers,
}

impl OmittedNullAssertions {
    fn omits(self, check: &NullCheck) -> bool {
        match self {
            Self::Implicit => check.is_implicit(),
            Self::ExtensionReceivers => check.guards_extension_receiver(),
        }
    }
}

/// Remove the implicit casts `omitted` selects. A cast is an expression wrapper, so it is removed
/// by rewriting the node into its operand's value.
pub(super) fn omit(ir: &mut IrFile, omitted: OmittedNullAssertions) {
    for expr in &mut ir.exprs {
        let IrExpr::NotNullAssert { operand, check } = expr else {
            continue;
        };
        if omitted.omits(check) {
            *expr = IrExpr::Block {
                stmts: Vec::new(),
                value: Some(*operand),
            };
        }
    }
}
