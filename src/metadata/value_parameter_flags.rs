//! What a function's value parameter declared, as `ValueParameter.flags` (f1) records it. The
//! writer and the classpath reader both go through these bits.

use crate::types::InlineParameterModifier;

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
    pub inline_modifier: InlineParameterModifier,
}

impl DeclaredValueParameter {
    pub const fn defaulted(declares_default: bool) -> Self {
        Self {
            declares_default,
            inline_modifier: InlineParameterModifier::None,
        }
    }

    /// The facts `flags` records.
    pub(crate) const fn of_flags(flags: u64) -> Self {
        Self {
            declares_default: flags & DECLARES_DEFAULT_VALUE != 0,
            inline_modifier: if flags & IS_NOINLINE != 0 {
                InlineParameterModifier::Noinline
            } else if flags & IS_CROSSINLINE != 0 {
                InlineParameterModifier::Crossinline
            } else {
                InlineParameterModifier::None
            },
        }
    }

    pub(crate) const fn flags(self) -> u64 {
        (if self.declares_default {
            DECLARES_DEFAULT_VALUE
        } else {
            0
        }) | match self.inline_modifier {
            InlineParameterModifier::None => 0,
            InlineParameterModifier::Crossinline => IS_CROSSINLINE,
            InlineParameterModifier::Noinline => IS_NOINLINE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_parameter_reads_back_from_its_flags() {
        for declares_default in [false, true] {
            for inline_modifier in [
                InlineParameterModifier::None,
                InlineParameterModifier::Crossinline,
                InlineParameterModifier::Noinline,
            ] {
                let declared = DeclaredValueParameter {
                    declares_default,
                    inline_modifier,
                };
                assert_eq!(DeclaredValueParameter::of_flags(declared.flags()), declared);
            }
        }
    }

    #[test]
    fn the_annotation_bit_is_not_a_declared_fact() {
        assert_eq!(
            DeclaredValueParameter::of_flags(1 | IS_CROSSINLINE),
            DeclaredValueParameter {
                declares_default: false,
                inline_modifier: InlineParameterModifier::Crossinline,
            }
        );
    }
}
