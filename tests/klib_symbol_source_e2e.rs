//! A KLIB as a [`SymbolSource`] — declarations from the library a non-JVM target actually ships.
//!
//! This is what the klib work was for. `KlibSymbols` answers the same questions `JvmLibraries`
//! answers, from a klib instead of a classpath, with no backend in the path: the packages a
//! qualifier walk needs, the classifiers a type reference resolves to, and the overloads a call
//! selects among.

use krusty::klib_symbols::KlibSymbols;
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::type_name;

fn native_stdlib() -> Option<KlibSymbols> {
    let stdlib = krusty::toolchain::kotlin_native_stdlib()?;
    Some(KlibSymbols::open(&[stdlib]))
}

#[test]
fn a_qualifier_walk_finds_the_packages_a_klib_declares() {
    let Some(symbols) = native_stdlib() else {
        return;
    };
    assert!(
        symbols.package_exists(type_name(""), "kotlin"),
        "the root segment of every stdlib package"
    );
    let kotlin = type_name("kotlin");
    assert!(symbols.package_exists(kotlin, "collections"));
    assert!(symbols.package_exists(kotlin, "native"));
    // An intermediate package that declares nothing itself still has to exist, or a fully
    // qualified reference through it has no way to know its leading segments are a package.
    assert!(
        symbols.package_exists(type_name("kotlin/native"), "concurrent"),
        "kotlin.native.concurrent, reached one segment at a time"
    );
    assert!(
        !symbols.package_exists(kotlin, "definitely_not_a_package"),
        "and a name no klib declares is not one"
    );
}

#[test]
fn a_classifier_resolves_with_its_declared_shape() {
    let Some(symbols) = native_stdlib() else {
        return;
    };
    let list = symbols
        .classifier(type_name("kotlin/collections/List"))
        .expect("kotlin.collections.List, from the Native stdlib klib");
    assert_eq!(list.kind, krusty::libraries::TypeKind::Interface);
    assert!(list.is_kotlin);
    assert!(
        list.inheritance.is_abstract && list.inheritance.is_extensible,
        "an interface is both"
    );
    assert_eq!(
        list.type_parameters.type_params(),
        &vec!["E".to_string()],
        "its element parameter, by source name"
    );
    assert!(
        list.supertypes
            .iter()
            .any(|supertype| supertype.matches("kotlin/collections/Collection")),
        "and its supertypes come from the decoded declaration"
    );
}

/// A member call's candidates, with the facts a descriptor erases.
///
/// A member resolves through the CLASSIFIER RECORD's `declared_callables`, which is the channel
/// every other provider publishes members on — not through a classifier-namespace probe. Both
/// halves are asserted, because a provider that filled only the probe would look right in isolation
/// and contribute nothing to a real member call.
#[test]
fn a_member_call_finds_its_overloads() {
    let Some(symbols) = native_stdlib() else {
        return;
    };
    let list = symbols
        .classifier(type_name("kotlin/collections/List"))
        .expect("kotlin.collections.List, from the Native stdlib klib");
    let functions = match list
        .declared_callables
        .get("get")
        .expect("List declares get")
    {
        krusty::libraries::Callables::Functions(functions)
        | krusty::libraries::Callables::Both { functions, .. } => functions,
        _ => panic!("List.get is a function"),
    };
    let get = functions
        .overloads
        .first()
        .expect("at least one List.get overload");
    assert_eq!(get.kind, krusty::libraries::FnKind::Member);
    assert_eq!(
        get.callable.params.len(),
        1,
        "get(index) takes one parameter"
    );
    assert!(
        matches!(get.callable.ret, krusty::types::Ty::TyParam(..)),
        "and returns the element TYPE PARAMETER, which a JVM descriptor erases to Object: {:?}",
        get.callable.ret
    );
    assert_eq!(
        get.call_sig.param_names,
        vec!["index".to_string()],
        "the parameter's source name, which a named argument needs"
    );
    assert!(
        list.declared_callable_order.contains(&"get".to_string()),
        "declaration order carries the name a materialized surface needs"
    );
    assert!(
        list.members.iter().any(|member| member.name == "get"),
        "and the member surface agrees with the lookup table"
    );

    assert!(
        matches!(
            symbols
                .symbols(
                    SymbolNamespace::Classifier(type_name("kotlin/collections/List")),
                    "get",
                )
                .callables,
            krusty::libraries::Callables::None
        ),
        "a classifier-namespace probe carries nested classifiers, not members"
    );
}

/// A member that declares its own type parameters composes them over its owner's.
#[test]
fn a_generic_member_keeps_its_own_type_parameters() {
    let Some(symbols) = native_stdlib() else {
        return;
    };
    let iterator = symbols
        .classifier(type_name("kotlin/collections/Iterator"))
        .expect("kotlin.collections.Iterator");
    let next = match iterator
        .declared_callables
        .get("next")
        .expect("Iterator declares next")
    {
        krusty::libraries::Callables::Functions(functions)
        | krusty::libraries::Callables::Both { functions, .. } => functions
            .overloads
            .first()
            .expect("one next overload")
            .clone(),
        _ => panic!("Iterator.next is a function"),
    };
    let signature = next
        .generic_sig
        .as_ref()
        .expect("every klib member publishes its declared signature");
    assert!(
        signature.formals.is_empty(),
        "next declares no type parameters of its own"
    );
    assert!(
        matches!(signature.ret, krusty::types::Ty::TyParam(..)),
        "and returns its owner's: {:?}",
        signature.ret
    );
}

/// A top-level function, and an extension separately, from the package namespace.
#[test]
fn a_top_level_call_finds_its_overloads() {
    let Some(symbols) = native_stdlib() else {
        return;
    };
    let record = symbols.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "listOf",
    );
    let functions = match &record.callables {
        krusty::libraries::Callables::Functions(functions)
        | krusty::libraries::Callables::Both { functions, .. } => functions,
        _ => panic!("listOf resolves to functions"),
    };
    assert!(
        functions.overloads.len() >= 2,
        "listOf is overloaded, got {}",
        functions.overloads.len()
    );
    assert!(
        functions
            .overloads
            .iter()
            .all(|overload| overload.receiver.is_none()),
        "and none of its overloads is an extension"
    );

    let is_nan = symbols.symbols(SymbolNamespace::Package(type_name("kotlin")), "isNaN");
    let extensions = match &is_nan.callables {
        krusty::libraries::Callables::Functions(functions)
        | krusty::libraries::Callables::Both { functions, .. } => functions
            .overloads
            .iter()
            .filter_map(|overload| {
                overload
                    .receiver
                    .and_then(|receiver| receiver.obj_internal())
                    .map(|internal| internal.render())
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert!(
        extensions.contains(&"kotlin/Double".to_string())
            && extensions.contains(&"kotlin/Float".to_string()),
        "isNaN is declared once per receiver — the member spelling the JVM facade does not have: \
         {extensions:?}"
    );
}
