//! `@OptionalExpectation` — an `expect` declaration a target may leave without an `actual`.
//!
//! The distinction matters in both directions: an ordinary `expect` with no `actual` is an error,
//! while an optional expectation is visible in common sources and erased where the target supplies
//! nothing. Only the `@OptionalExpectation` on the declaration says which, so that is what is read
//! rather than "an annotation class that is also `expect`".
//!
//! The Kotlin/Native stdlib ships 26 of them: the whole `kotlin.jvm.*` set and the whole
//! `kotlin.js.*` set, which is how `@JvmStatic` can appear in common code that also compiles for
//! Native.

use krusty::klib_symbols::{KlibSymbols, PlatformWithKlibs};
use krusty::libraries::SemanticPlatform;
use krusty::types::type_name;

#[test]
fn the_native_stdlib_s_optional_expectations_are_named_as_such() {
    let Some(stdlib) = krusty::toolchain::kotlin_native_stdlib() else {
        return;
    };
    let symbols = KlibSymbols::open(&[stdlib]);
    for name in [
        "kotlin/jvm/JvmStatic",
        "kotlin/jvm/JvmName",
        "kotlin/jvm/JvmField",
        "kotlin/jvm/JvmInline",
        "kotlin/js/JsExport",
        "kotlin/js/JsName",
    ] {
        assert!(
            symbols.is_optional_expectation(type_name(name)),
            "{name} is an @OptionalExpectation annotation with no Native actual"
        );
    }
    for name in [
        "kotlin/collections/List",
        "kotlin/Deprecated",
        "kotlin/UInt",
    ] {
        assert!(
            !symbols.is_optional_expectation(type_name(name)),
            "{name} is not one — a declaration the library actually supplies"
        );
    }
}

/// The platform federates the question, because whichever side declares the classifier is the side
/// that knows whether its `actual` is optional.
#[test]
fn a_platform_with_klibs_federates_the_question() {
    let Some(jar) = krusty::toolchain::stdlib_jar() else {
        return;
    };
    let Some(stdlib) = krusty::toolchain::kotlin_native_stdlib() else {
        return;
    };
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![jar]));
    let federated = PlatformWithKlibs::new(
        Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        KlibSymbols::open(&[stdlib]),
    );
    assert!(
        federated.is_optional_expectation(type_name("kotlin/js/JsExport")),
        "the klib's answer reaches the platform"
    );
    assert!(!federated.is_optional_expectation(type_name("kotlin/collections/List")));
}
