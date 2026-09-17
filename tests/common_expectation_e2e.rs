//! Optional `expect` annotations, which the JVM reads out of a KLIB.
//!
//! Kotlin ships the common `expect` headers in a klib beside `kotlin-stdlib.jar`, and an
//! `@OptionalExpectation` annotation with no JVM `actual` is visible in common sources and erased on
//! this target. That makes it the one classifier the JVM backend builds from a klib declaration
//! rather than from a class file — and the classifier record is now assembled by the core, so the
//! fields the JVM path used to set itself are pinned here.
//!
//! There was no test over this path before the record's assembly moved. Its absence is why the
//! `is_extensible` difference below had to be caught by reading.

use krusty::symbol_source::SymbolSource;

#[test]
fn an_optional_expectation_annotation_comes_from_the_klib() {
    let stdlib = krusty::toolchain::stdlib_jar();
    let Some(stdlib) = stdlib else {
        return;
    };
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![stdlib]));
    let libraries = krusty::jvm::jvm_libraries::JvmLibraries::new(classpath);

    let identity = krusty::types::type_name("kotlin/js/JsStatic");
    let Some(classifier) = libraries.classifier(identity) else {
        // The distribution's klib is what supplies this; without it there is nothing to assert.
        return;
    };

    assert_eq!(classifier.kind, krusty::libraries::TypeKind::Annotation);
    assert!(
        classifier.is_kotlin,
        "a klib declaration is a Kotlin declaration"
    );
    assert!(
        classifier.inheritance.is_abstract,
        "an annotation class is abstract"
    );
    assert!(
        !classifier.inheritance.is_extensible,
        "and cannot be subclassed — unlike an interface, which shares its abstractness"
    );
    assert_eq!(
        classifier.retention.as_deref(),
        Some("SOURCE"),
        "no JVM actual exists, so this platform erases it after checking"
    );
    assert!(
        classifier.supertypes.iter().next().is_some(),
        "its supertypes come from the decoded declaration"
    );
}
