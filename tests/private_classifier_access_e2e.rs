//! A `private` classifier is package-private in the class file, for every declaration kind.
//!
//! The JVM has no class-level `private`, so kotlinc drops `ACC_PUBLIC` and records the real
//! visibility in `@Metadata` (and, for a nested classifier, in `InnerClasses`). krusty stamped
//! `ACC_PUBLIC` on all of them: only the `class` arm of the top-level declaration parser recorded
//! the modifier at all, so `private object`/`interface`/`enum class`/`annotation class` reached the
//! backend claiming to be public — a hidden declaration that any other module could link against.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and each
//! class's access flags are compared.

use super::common;

/// The `flags:` line of a class's own access flags from `javap -v` (NOT a member's — the class entry
/// is the one that follows the `this_class` header).
fn class_flags(dir: &std::path::Path, class: &str) -> String {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-v", "-p", &path.to_string_lossy()]).expect("pooled javap");
    raw.lines()
        .find(|line| line.starts_with("  flags: "))
        .unwrap_or_else(|| panic!("no class flags for {class}"))
        .trim()
        .to_string()
}

/// Every declaration kind, private and (as a control) internal — which stays public, since the
/// module boundary is a Kotlin-only fact.
const KINDS: &str = r#"
private class PrivClass
private data class PrivData(val value: Int)
private object PrivObject
private data object PrivDataObject
private interface PrivInterface
private fun interface PrivFunInterface { fun run() }
private sealed class PrivSealed
private sealed interface PrivSealedInterface
private enum class PrivEnum { ONE }
private annotation class PrivAnnotation
@JvmInline private value class PrivValue(val value: Int)
internal class InternalClass
fun localFactory(): Any {
    class LocalClassifier
    return LocalClassifier()
}
open class Outer {
    private class Nested
    private inner class Inner
    private object NestedObject
    private interface NestedInterface
    private enum class NestedEnum { ONE }
    private annotation class NestedAnnotation
    // A PROTECTED classifier keeps ACC_PUBLIC: the JVM class access flags cannot express protection
    // at all, so kotlinc records it only in `InnerClasses`.
    protected class Protected
    protected object ProtectedObject
}
"#;

#[test]
fn a_private_classifier_is_package_private_for_every_kind() {
    let build = common::compile_libs_build("private_classifier_access", &[("Hidden.kt", KINDS)])
        .expect("scratch directory for classifier fixture");
    let Some(kotlinc_dir) = build.reference_out() else {
        return; // toolchain not provisioned
    };
    let krusty_dir = build.krusty_out();
    for class in [
        "PrivClass",
        "PrivData",
        "PrivObject",
        "PrivDataObject",
        "PrivInterface",
        "PrivFunInterface",
        "PrivSealed",
        "PrivSealedInterface",
        "PrivEnum",
        "PrivAnnotation",
        "PrivValue",
        "InternalClass",
        "Outer",
        "Outer$Nested",
        "Outer$Inner",
        "Outer$NestedObject",
        "Outer$NestedInterface",
        "Outer$NestedEnum",
        "Outer$NestedAnnotation",
        "Outer$Protected",
        "Outer$ProtectedObject",
    ] {
        assert_eq!(
            class_flags(krusty_dir, class),
            class_flags(kotlinc_dir, class),
            "{class}: class access flags must match kotlinc's"
        );
    }
    // Guard the comparison: the private kinds must actually have LOST the public bit, and the
    // internal control must have kept it — an all-equal-but-wrong pair would pass vacuously.
    assert!(
        !class_flags(krusty_dir, "PrivClass").contains("ACC_PUBLIC"),
        "a private class must not be ACC_PUBLIC"
    );
    assert!(
        class_flags(krusty_dir, "InternalClass").contains("ACC_PUBLIC"),
        "an internal class stays ACC_PUBLIC"
    );
    assert!(
        !class_flags(krusty_dir, "Outer$NestedObject").contains("ACC_PUBLIC"),
        "a private nested object must not be ACC_PUBLIC"
    );
    assert!(
        class_flags(krusty_dir, "Outer$Protected").contains("ACC_PUBLIC"),
        "a protected classifier keeps ACC_PUBLIC — the JVM cannot spell protection on a class"
    );
    assert!(
        class_flags(krusty_dir, "localFactory$LocalClassifier").contains("ACC_PUBLIC"),
        "a local classifier keeps ACC_PUBLIC, as kotlinc emits it"
    );
    assert!(
        class_flags(kotlinc_dir, "HiddenKt$localFactory$LocalClassifier").contains("ACC_PUBLIC"),
        "the kotlinc oracle must demonstrate the local-class control"
    );
}
