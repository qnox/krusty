//! What a target chooses about the shape of the target-neutral lowering into common IR.

/// Choices the target makes about the shape of otherwise target-neutral lowerings, in the way
/// kotlinc's `CommonBackendContext` exposes them to its common lowerings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CommonLoweringOptions {
    /// kotlinc's `preferJavaLikeCounterLoop`: a counted loop whose bound can be made exclusive is
    /// lowered to the shape of a Java `for (int i = first; i < last; ++i)` loop, because HotSpot
    /// only recognizes that shape as a counter loop.
    pub prefer_java_like_counter_loop: bool,
}
