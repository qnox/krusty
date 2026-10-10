//! Reusable fixtures for tests that build common IR by hand.

use super::IrClass;

/// A minimal well-formed `IrClass` for tests that build IR by hand and only exercise fields/functions.
pub(crate) fn blank_class(fq: &str) -> IrClass {
    IrClass::generated(fq.into())
}
