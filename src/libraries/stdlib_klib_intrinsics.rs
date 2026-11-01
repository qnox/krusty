//! Compiler operations authenticated by an exact Kotlin common-stdlib KLIB identity.
//!
//! These declarations have shape-equivalent lookalikes, so package/name/signature facts alone do
//! not grant their operation. JVM pairs the trusted common KLIB identities with its physical
//! stdlib methods; KLIB targets consume the same identities directly from their selected platform
//! stdlib.

use super::{CompilerIntrinsic, FnKind, FunctionInfo, GenericSig, InlineKind};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::types::{Ty, TypeName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StdlibKlibIntrinsic {
    DoubleRangeTo,
    FloatRangeTo,
    DoubleRangeUntil,
    FloatRangeUntil,
    EnumEntries,
}

impl StdlibKlibIntrinsic {
    /// Every identity the common stdlib KLIB can carry a role for.
    pub(crate) const ALL: [Self; 5] = [
        Self::DoubleRangeTo,
        Self::FloatRangeTo,
        Self::DoubleRangeUntil,
        Self::FloatRangeUntil,
        Self::EnumEntries,
    ];

    pub(crate) fn compiler_intrinsic(self) -> CompilerIntrinsic {
        match self {
            Self::DoubleRangeTo
            | Self::FloatRangeTo
            | Self::DoubleRangeUntil
            | Self::FloatRangeUntil => CompilerIntrinsic::FloatingRangeMembership,
            Self::EnumEntries => CompilerIntrinsic::EnumEntries,
        }
    }

    /// Exact source shape used to join a common identity to its paired JVM realization.
    pub(crate) fn accepts_source(self, candidate: &FunctionInfo) -> bool {
        candidate.generic_sig.as_ref() == Some(&self.generic_signature())
            && self.accepts_flags(candidate)
    }

    /// Exact shape after the target-KLIB provider has assigned declaration-owned formal identities
    /// and normalized their bounds.
    pub(crate) fn accepts_normalized_klib(self, candidate: &FunctionInfo) -> bool {
        let Some(signature) = candidate.generic_sig.as_ref() else {
            return false;
        };
        let expected = match self {
            Self::EnumEntries => {
                // KLIB normalization gives each declaration an opaque formal identity. Project
                // the expected one-formal shape through that identity so the comparison is alpha
                // equivalent while every bound and use of the formal remains exact. Its normalized
                // formal-bound entry has already applied the primary bound to the nested `T` in
                // `Enum<T>`; this is deliberately distinct from the source-shaped JVM join.
                let [formal] = signature.formals.as_slice() else {
                    return false;
                };
                normalized_enum_entries_signature(formal)
            }
            _ => self.generic_signature(),
        };
        if signature != &expected {
            return false;
        }
        self.accepts_flags(candidate)
    }

    /// Refuse a corrupt declaration even when its serialized identity names a compiler operation.
    fn accepts_flags(self, candidate: &FunctionInfo) -> bool {
        let plain = !candidate.flags.suspend
            && !candidate.flags.infix
            && candidate.context_count == 0
            && !candidate.call_sig.vararg;
        match self {
            Self::DoubleRangeTo
            | Self::FloatRangeTo
            | Self::DoubleRangeUntil
            | Self::FloatRangeUntil => {
                plain && candidate.kind == FnKind::Extension && candidate.flags.operator
            }
            Self::EnumEntries => {
                plain
                    && candidate.kind == FnKind::TopLevel
                    && !candidate.flags.operator
                    && candidate.flags.reified
                    && candidate.flags.inline == InlineKind::MustInline
            }
        }
    }

    pub(crate) fn package(self) -> TypeName {
        match self {
            Self::EnumEntries => crate::types::type_name("kotlin/enums"),
            Self::DoubleRangeTo
            | Self::FloatRangeTo
            | Self::DoubleRangeUntil
            | Self::FloatRangeUntil => crate::types::wk::kotlin_ranges_package(),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::DoubleRangeTo | Self::FloatRangeTo => "rangeTo",
            Self::DoubleRangeUntil | Self::FloatRangeUntil => "rangeUntil",
            Self::EnumEntries => "enumEntries",
        }
    }

    /// Complete source signature owned by the authenticated common-stdlib identity.
    pub(crate) fn generic_signature(self) -> GenericSig {
        let (scalar, range) = match self {
            Self::DoubleRangeTo => (Ty::Double, "kotlin/ranges/ClosedFloatingPointRange"),
            Self::FloatRangeTo => (Ty::Float, "kotlin/ranges/ClosedFloatingPointRange"),
            Self::DoubleRangeUntil => (Ty::Double, "kotlin/ranges/OpenEndRange"),
            Self::FloatRangeUntil => (Ty::Float, "kotlin/ranges/OpenEndRange"),
            Self::EnumEntries => return enum_entries_signature("T"),
        };
        GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: Some(scalar),
            params: vec![scalar],
            ret: Ty::obj_args(range, &[scalar]),
            return_policy: Default::default(),
        }
    }
}

fn enum_entries_signature(formal: &str) -> GenericSig {
    let parameter_in_bound = Ty::ty_param(formal, Ty::nullable(Ty::obj("kotlin/Any")));
    let bound = Ty::obj_args("kotlin/Enum", &[parameter_in_bound]);
    let parameter = Ty::ty_param(formal, bound);
    GenericSig {
        formals: vec![formal.to_string()],
        formal_bounds: vec![vec![bound]],
        receiver: None,
        params: Vec::new(),
        ret: Ty::obj_args("kotlin/enums/EnumEntries", &[parameter]),
        return_policy: Default::default(),
    }
}

/// Provider-normalized shape of `<T : Enum<T>> enumEntries(): EnumEntries<T>`.
///
/// [`enum_entries_signature`] is the source shape used to join a common identity to the paired JVM
/// declaration. KLIB candidates have passed through `declared_generic_sig`: their primary bound is
/// first computed as `Enum<T[Any?]>`, then that bound is applied when the declared bound itself is
/// converted. Preserve that exact representation instead of making generic-signature equality
/// ignore bounds.
fn normalized_enum_entries_signature(formal: &str) -> GenericSig {
    let unbounded = Ty::ty_param(formal, Ty::nullable(Ty::obj("kotlin/Any")));
    let primary_bound = Ty::obj_args("kotlin/Enum", &[unbounded]);
    let parameter = Ty::ty_param(formal, primary_bound);
    let declared_bound = Ty::obj_args("kotlin/Enum", &[parameter]);
    GenericSig {
        formals: vec![formal.to_string()],
        formal_bounds: vec![vec![declared_bound]],
        receiver: None,
        params: Vec::new(),
        ret: Ty::obj_args("kotlin/enums/EnumEntries", &[parameter]),
        return_policy: Default::default(),
    }
}

/// The compiler operation carried by one exact public identity in Kotlin's common stdlib KLIB.
pub(crate) fn stdlib_klib_intrinsic(
    identity: &KlibPublicIdSignature,
) -> Option<StdlibKlibIntrinsic> {
    const DOUBLE_RANGE_TO: u64 = 692_997_638_542_153_957;
    const FLOAT_RANGE_TO: u64 = 14_812_996_858_166_169_491;
    const DOUBLE_RANGE_UNTIL: u64 = 742_649_461_109_916_381;
    const FLOAT_RANGE_UNTIL: u64 = 1_543_348_898_644_284_516;
    const ENUM_ENTRIES: u64 = 1_918_022_784_687_648_499;

    if identity.matches_exact(&["kotlin", "enums"], &["enumEntries"], ENUM_ENTRIES, 0) {
        Some(StdlibKlibIntrinsic::EnumEntries)
    } else if identity.matches_exact(&["kotlin", "ranges"], &["rangeTo"], DOUBLE_RANGE_TO, 0) {
        Some(StdlibKlibIntrinsic::DoubleRangeTo)
    } else if identity.matches_exact(&["kotlin", "ranges"], &["rangeTo"], FLOAT_RANGE_TO, 0) {
        Some(StdlibKlibIntrinsic::FloatRangeTo)
    } else if identity.matches_exact(
        &["kotlin", "ranges"],
        &["rangeUntil"],
        DOUBLE_RANGE_UNTIL,
        0,
    ) {
        Some(StdlibKlibIntrinsic::DoubleRangeUntil)
    } else if identity.matches_exact(&["kotlin", "ranges"], &["rangeUntil"], FLOAT_RANGE_UNTIL, 0) {
        Some(StdlibKlibIntrinsic::FloatRangeUntil)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::LibraryCallable;
    use crate::types::type_name;

    fn candidate(role: StdlibKlibIntrinsic) -> FunctionInfo {
        let signature = role.generic_signature();
        let kind = if signature.receiver.is_some() {
            FnKind::Extension
        } else {
            FnKind::TopLevel
        };
        let mut candidate = FunctionInfo::plain(
            kind,
            signature.receiver,
            LibraryCallable::library(
                type_name("kotlin/TestKt"),
                role.name(),
                signature.params.clone(),
                signature.ret,
                signature.ret,
                "",
            ),
        );
        candidate.generic_sig = Some(signature);
        match role {
            StdlibKlibIntrinsic::EnumEntries => {
                candidate.flags.reified = true;
                candidate.flags.inline = InlineKind::MustInline;
            }
            _ => candidate.flags.operator = true,
        }
        candidate
    }

    #[test]
    fn floating_range_identity_requires_its_complete_semantic_shape() {
        let role = StdlibKlibIntrinsic::DoubleRangeTo;
        let exact = candidate(role);
        assert!(role.accepts_source(&exact));

        let mut wrong_receiver = exact.clone();
        wrong_receiver.generic_sig.as_mut().unwrap().receiver = Some(Ty::Float);
        assert!(!role.accepts_source(&wrong_receiver));

        let mut wrong_parameter = exact.clone();
        wrong_parameter.generic_sig.as_mut().unwrap().params = vec![Ty::Float];
        assert!(!role.accepts_source(&wrong_parameter));

        let mut wrong_result = exact;
        wrong_result.generic_sig.as_mut().unwrap().ret =
            Ty::obj_args("kotlin/ranges/OpenEndRange", &[Ty::Double]);
        assert!(!role.accepts_source(&wrong_result));
    }
}
