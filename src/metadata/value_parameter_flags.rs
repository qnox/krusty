//! What a function's value parameter declared, as `ValueParameter.flags` (f1) records it.

/// `ValueParameter.flags` bit 1: the parameter writes a default value, so a caller may omit it.
const DECLARES_DEFAULT_VALUE: u64 = 1 << 1;
/// Bit 2: the parameter wrote `crossinline`.
const IS_CROSSINLINE: u64 = 1 << 2;
/// Bit 3: the parameter wrote `noinline`.
const IS_NOINLINE: u64 = 1 << 3;

/// The declaration facts one value parameter records. `HAS_ANNOTATIONS` (bit 0) is not here: it
/// follows from the annotation records the writer emits beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeclaredValueParameter {
    pub declares_default: bool,
    pub crossinline: bool,
    pub noinline: bool,
}

impl DeclaredValueParameter {
    pub const fn defaulted(declares_default: bool) -> Self {
        Self {
            declares_default,
            crossinline: false,
            noinline: false,
        }
    }

    pub(crate) const fn flags(self) -> u64 {
        (if self.declares_default {
            DECLARES_DEFAULT_VALUE
        } else {
            0
        }) | (if self.crossinline { IS_CROSSINLINE } else { 0 })
            | (if self.noinline { IS_NOINLINE } else { 0 })
    }
}
