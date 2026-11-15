//! The strict-equality contract a checked `equals` body opens with.

use super::{FirBody, LocalValueId, ResolvedTy};

/// The `@EqualityBound` contract an `equals` body was checked under: the bound value of its
/// parameter and the classifier type the body reads that parameter as. The bound is either
/// declared on the parameter or inherited from the overridden `equals`.
///
/// The body may read the parameter as `bound` only because the call never reaches the source
/// statements with any other value. Common lowering realizes that guarantee as kotlinc's fir2ir
/// does: `if (this === other) return true` (outside a value class), then
/// `if (other !is Bound) return false`, ahead of the source body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirEqualityBoundGuard {
    pub parameter: LocalValueId,
    /// The parameter's ordinal among the declaration's parameters, context parameters first. The
    /// declaration's `@Metadata` records the bound on that parameter.
    pub ordinal: u32,
    pub bound: ResolvedTy,
}

impl FirBody {
    pub fn publish_equality_bound_guard(&mut self, guard: FirEqualityBoundGuard) {
        assert!(
            self.equality_bound_guard.is_none(),
            "an equals body is checked under one equality bound"
        );
        self.equality_bound_guard = Some(guard);
    }

    pub fn equality_bound_guard(&self) -> Option<FirEqualityBoundGuard> {
        self.equality_bound_guard
    }
}
