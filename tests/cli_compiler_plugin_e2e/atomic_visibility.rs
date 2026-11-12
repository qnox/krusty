//! Atomicfu's declaration checks agree with the official plugin before representation lowering.

use super::common;

#[test]
fn atomic_publication_uses_checked_visibility_annotations_and_exact_declaration_sites() {
    let source = r#"package probe
import kotlinx.atomicfu.atomic
import kotlin.PublishedApi as Exposed

annotation class PublishedApi

class PublicOwner {
    val implicit = atomic(0)
    public val explicit = atomic(0L)
    @kotlin.PublishedApi
    internal val published = atomic(0)
    @Exposed internal val aliased = atomic(0)
    @PublishedApi internal val decoy = atomic(0)
    public var mutable = atomic(0)
    public val boolean = atomic(false)
}
open class ProtectedOwner {
    protected val guarded = atomic(0)
}
@kotlin.PublishedApi internal class PublishedOwner {
    val member = atomic(0)
}
val top = atomic(0)
class ConstructorOwner(val atomic: kotlinx.atomicfu.AtomicInt)
class ExplicitConstructorOwner(public var atomic: kotlinx.atomicfu.AtomicInt)
internal class InternalOwner {
    val allowed = atomic(0)
    @kotlin.PublishedApi internal val alsoAllowed = atomic(0)
    class Nested { val exposed = atomic(0) }
}
private class PrivateOwner {
    val allowed = atomic(0L)
    @kotlin.PublishedApi internal val alsoAllowed = atomic(0)
}
fun localOwner() {
    class Local { val allowed = atomic(0) }
}
"#;
    let plugin = krusty::toolchain::atomicfu_compiler_plugin_jar()
        .expect("the atomicfu compiler plugin is provisioned");
    let result = common::compiler_diagnostics_with_shared_args(
        &[("AtomicVisibility.kt", source)],
        &[
            krusty::toolchain::atomicfu_runtime_jar().expect("the atomicfu runtime is provisioned"),
            common::stdlib_jar(),
        ],
        &[format!("-Xplugin={}", plugin.display())],
    );
    common::expect_identical_rejection(&result, "atomic publication");
    assert_eq!(common::compiler_errors(&result.reference_stderr).len(), 14);
    let expected = common::rendered_error_blocks(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "the official plugin emits complete diagnostics"
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stdout, false),
        Vec::<String>::new()
    );
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        expected
    );
}
