//! A platform whose dependency path includes KLIBs.
//!
//! `PlatformWithKlibs` is the seam that makes klib ingestion reach resolution: it federates the
//! klib's declarations under the wrapped platform's own libraries and passes every other platform
//! question straight through. Both halves are asserted here, because a wrapper that answered the
//! trait default for a delegated method would fail silently — the platform *has* an answer, and the
//! default is "no platform".

use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::klib_symbols::{KlibSymbols, PlatformWithKlibs};
use krusty::libraries::SemanticPlatform;
use krusty::symbol_source::SymbolSource;
use krusty::types::{type_name, Ty};

/// The JVM platform alone, and the same platform with the Native stdlib klib federated under it.
/// `None` when either library is absent from this distribution.
fn platforms() -> Option<(JvmLibraries, PlatformWithKlibs)> {
    let stdlib_jar = krusty::toolchain::stdlib_jar()?;
    let stdlib_klib = krusty::toolchain::kotlin_native_stdlib()?;
    let classpath = std::rc::Rc::new(Classpath::new(vec![stdlib_jar]));
    Some((
        JvmLibraries::new(classpath.clone()),
        PlatformWithKlibs::new(
            Box::new(JvmLibraries::new(classpath)),
            KlibSymbols::open(&[stdlib_klib]),
        ),
    ))
}

/// A package only the klib declares becomes walkable, without the JVM losing its own.
#[test]
fn a_package_only_a_klib_declares_joins_the_qualifier_walk() {
    let Some((jvm, federated)) = platforms() else {
        return;
    };
    let native = type_name("kotlin/native");
    assert!(
        !jvm.package_exists(native, "concurrent"),
        "the JVM stdlib jar has no kotlin.native.concurrent — otherwise this proves nothing"
    );
    assert!(
        federated.package_exists(native, "concurrent"),
        "and the klib supplies it"
    );
    assert!(
        federated.package_exists(type_name("kotlin"), "jvm"),
        "while the platform's own packages are still there"
    );
}

/// A classifier only the klib declares resolves, members and all.
#[test]
fn a_classifier_only_a_klib_declares_resolves_through_the_platform() {
    let Some((jvm, federated)) = platforms() else {
        return;
    };
    let weak_reference = type_name("kotlin/native/ref/WeakReference");
    assert!(
        jvm.classifier(weak_reference).is_none(),
        "the JVM stdlib jar has no kotlin.native.ref.WeakReference — otherwise this proves nothing"
    );
    let declared = federated
        .classifier(weak_reference)
        .expect("and the klib supplies it");
    assert!(
        declared.declared_callables.contains_key("get"),
        "with the member surface a call resolves against: {:?}",
        declared.declared_callable_order
    );
}

/// Where both sides declare the same name the platform's record wins WHOLE, the precedence an extra
/// classpath entry already has: a classifier is one declaration, so its member surface is not
/// blended across providers.
#[test]
fn the_platform_shadows_a_name_the_klib_also_declares() {
    let Some((jvm, federated)) = platforms() else {
        return;
    };
    let list = type_name("kotlin/collections/List");
    let (Some(platform_only), Some(merged)) = (jvm.classifier(list), federated.classifier(list))
    else {
        return;
    };
    assert_eq!(platform_only.kind, merged.kind);
    let descriptors = |classifier: &krusty::libraries::LibraryType| match classifier
        .declared_callables
        .get("get")
    {
        Some(krusty::libraries::Callables::Functions(functions))
        | Some(krusty::libraries::Callables::Both { functions, .. }) => functions
            .overloads
            .iter()
            .map(|overload| overload.callable.descriptor.clone())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert_eq!(
        descriptors(&merged),
        descriptors(&platform_only),
        "List.get comes from the platform, descriptors and all"
    );
    assert!(
        descriptors(&merged).iter().all(|d| !d.is_empty()),
        "and no klib overload leaked in carrying none: {:?}",
        descriptors(&merged)
    );
}

/// Everything that is not a declaration lookup is the wrapped platform's answer, verbatim.
#[test]
fn every_platform_semantic_is_the_wrapped_platform_s() {
    let Some((jvm, federated)) = platforms() else {
        return;
    };
    assert_eq!(
        federated.function_type(1),
        jvm.function_type(1),
        "a function value's semantic interface"
    );
    assert!(
        federated.function_type(1).is_some(),
        "which the trait default would have answered None for"
    );
    assert_eq!(federated.class_literal_type(), jvm.class_literal_type());
    assert_eq!(
        federated.platform_default_import_packages(),
        jvm.platform_default_import_packages(),
    );
    assert!(
        !federated.platform_default_import_packages().is_empty(),
        "the JVM's default imports, not an empty default"
    );
    let int = Ty::obj_name(type_name("kotlin/Int"));
    assert_eq!(federated.boxed_primitive(int), jvm.boxed_primitive(int));
    assert_eq!(
        federated.builtin_type_internal("String"),
        jvm.builtin_type_internal("String"),
    );
    assert_eq!(
        federated.iterable_element_type_name(type_name("kotlin/collections/List")),
        jvm.iterable_element_type_name(type_name("kotlin/collections/List")),
    );
    assert_eq!(
        federated.platform_flexible_upper_bound(int),
        jvm.platform_flexible_upper_bound(int),
    );
}
