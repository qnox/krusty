//! The JVM class a lambda realized as a class of its own ([`LambdaMode::Class`]) is written to.

use super::*;

/// The class name and identity of the lambda implemented by `impl_fn`, whose implementation
/// method `impl_name` lives on `impl_owner`.
pub(super) fn class_name(
    ir: &IrFile,
    impl_fn: u32,
    impl_name: &str,
    impl_owner: &str,
) -> (String, LambdaClassIdentity) {
    // Common lowering records the source lambda's stable lexical origin. Consume that edge
    // directly: a source lambda lowered into multiple constructors keeps one name and identity,
    // and no generated method spelling or value table is searched here.
    let origin = ir.lambda_origins.get(&impl_fn);
    // The source's naming walk names a source lambda's class where it nests, as kotlinc does
    // (`Kt$box$1$1` inside `Kt$box$1`), whether or not its enclosing lambda became a class of its
    // own.
    if let Some(name) = crate::jvm::local_class_names::lambda_class_name(ir, impl_fn) {
        let identity = origin.map_or(LambdaClassIdentity::Synthetic(impl_fn), |origin| {
            LambdaClassIdentity::Source(origin.identity)
        });
        return (name.render(), identity);
    }
    if let Some(origin) = origin {
        let ordinal = origin.ordinal + 1;
        // A class-initialization origin (a property initializer or an init block) has the EMPTY
        // enclosing name — kotlinc emits no function segment there (`C$prop$1`, `C$local$1`,
        // `C$1`), so an empty segment is dropped, never printed as `C$$1`.
        let mut internal = impl_owner.to_owned();
        for segment in [
            Some(origin.enclosing_name.as_str()),
            origin.binding_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        {
            internal.push('$');
            internal.push_str(segment);
        }
        internal.push('$');
        internal.push_str(&ordinal.to_string());
        return (internal, LambdaClassIdentity::Source(origin.identity));
    }
    // Backend-synthesized lambdas have no source expression. Their implementation id is already
    // the exact stable identity; its generated name is serialization input only for this JVM
    // artifact boundary.
    let (enclosing, index) = impl_name
        .split_once("$lambda$")
        .map(|(head, tail)| (head.to_string(), tail.parse::<u32>().unwrap_or(0)))
        .unwrap_or_else(|| (impl_name.to_owned(), 0));
    (
        format!("{impl_owner}${enclosing}${}", index + 1),
        LambdaClassIdentity::Synthetic(impl_fn),
    )
}
