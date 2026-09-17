//! What a KLIB DECLARES, read through the core alone.
//!
//! This is the test the decoder's move exists for. `KlibArchive` opens the container, the Kotlin
//! metadata reader decodes its `linkdata` fragments, and neither step goes through a backend — so a
//! non-JVM target can take its signatures from the library Kotlin ships instead of inferring them
//! from the JVM stdlib jar, or from a hand-written table.
//!
//! The assertions are facts about the shipped `kotlin-stdlib-js.klib`, measured rather than assumed:
//! full Kotlin signatures, with type parameters, type arguments, extension receivers, nullability
//! and `inline` intact — everything a resolver needs and a JVM descriptor has already erased.

use krusty::klib::KlibArchive;
use krusty::metadata::reader::{parse_package_fragment, BuiltinClass, BuiltinFunction};

use crate::common;

fn declarations_of(
    archive: &KlibArchive,
) -> (
    std::collections::HashMap<String, BuiltinClass>,
    Vec<BuiltinFunction>,
) {
    let mut classes = std::collections::HashMap::new();
    let mut functions = Vec::new();
    for fragment in archive.package_fragments() {
        let Some(bytes) = archive.read(&fragment.entry) else {
            continue;
        };
        let package = parse_package_fragment(&bytes);
        classes.extend(package.classes);
        functions.extend(package.functions);
    }
    (classes, functions)
}

fn stdlib_declarations() -> Option<(
    std::collections::HashMap<String, BuiltinClass>,
    Vec<BuiltinFunction>,
)> {
    let directory = krusty::toolchain::kotlinc_lib_dir()?;
    let path = directory.join("kotlin-stdlib-js.klib");
    if !path.is_file() {
        return None;
    }
    Some(declarations_of(&KlibArchive::open(&path)?))
}

/// The whole stdlib API surface is in there, not a handful of headers. The counts are a floor rather
/// than an equality so a newer distribution declaring more does not fail the test — what would fail
/// it is a decode that silently stops early, which is the failure this guards.
#[test]
fn the_stdlib_klib_declares_its_whole_api() {
    let Some((classes, functions)) = stdlib_declarations() else {
        return;
    };
    assert!(
        classes.len() > 500,
        "the stdlib klib declares hundreds of classifiers, got {}",
        classes.len()
    );
    assert!(
        functions.len() > 4000,
        "and thousands of top-level functions, got {}",
        functions.len()
    );
    for expected in [
        "kotlin/Any",
        "kotlin/String",
        "kotlin/Int",
        "kotlin/CharSequence",
        "kotlin/collections/Collection",
        "kotlin/collections/List",
    ] {
        assert!(
            classes.contains_key(expected),
            "{expected} is declared: {} classifiers",
            classes.len()
        );
    }
}

/// A classifier arrives with the facts a resolver asks of it: its kind, its type parameters, its
/// supertypes and its members' declared (unerased) signatures.
#[test]
fn a_classifier_arrives_with_its_declared_shape() {
    let Some((classes, _)) = stdlib_declarations() else {
        return;
    };
    let list = classes
        .get("kotlin/collections/List")
        .expect("kotlin.collections.List is declared");
    assert_eq!(list.kind, krusty::libraries::TypeKind::Interface);
    assert_eq!(list.visibility, krusty::types::Visibility::Public);
    assert_eq!(
        list.type_params
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>(),
        vec!["E"],
        "the element parameter, by its source name"
    );
    assert_eq!(list.supertypes, vec!["kotlin/collections/Collection"]);

    let get = list
        .members
        .iter()
        .find(|member| member.name == "get")
        .expect("List.get");
    assert_eq!(
        get.params
            .iter()
            .map(|parameter| parameter.render())
            .collect::<Vec<_>>(),
        vec!["kotlin/Int"]
    );
    assert_eq!(
        get.ret.render(),
        "E",
        "the return is the type PARAMETER, which a JVM descriptor would have erased to Object"
    );

    let mutable = classes
        .get("kotlin/collections/MutableList")
        .expect("kotlin.collections.MutableList is declared");
    assert_eq!(
        mutable.supertypes,
        vec![
            "kotlin/collections/List",
            "kotlin/collections/MutableCollection"
        ],
        "the read-only/mutable split is a fact of the metadata, not a curated table"
    );
}

/// A top-level function arrives with its extension receiver, its type arguments and its `inline`
/// modifier — the three things the JVM jar's descriptors cannot tell a resolver.
#[test]
fn a_top_level_function_arrives_with_its_receiver_and_generics() {
    let Some((_, functions)) = stdlib_declarations() else {
        return;
    };
    let list_of: Vec<&BuiltinFunction> = functions
        .iter()
        .filter(|function| function.name == "listOf")
        .collect();
    assert!(
        list_of.len() >= 3,
        "listOf is overloaded (vararg, single element, empty), got {}",
        list_of.len()
    );
    assert!(
        list_of
            .iter()
            .any(|function| function.params.is_empty() && function.is_inline),
        "the empty overload is inline: {:?}",
        list_of
            .iter()
            .map(|function| (function.params.len(), function.is_inline))
            .collect::<Vec<_>>()
    );
    assert!(
        list_of
            .iter()
            .any(|function| function.ret.render() == "kotlin/collections/List<T>"),
        "and each returns List<T>, type argument included"
    );

    let joined = functions
        .iter()
        .filter(|function| function.name == "joinToString")
        .filter_map(|function| function.receiver.as_ref().map(|receiver| receiver.render()))
        .collect::<Vec<_>>();
    assert!(
        joined.contains(&"kotlin/collections/Iterable<T>".to_string()),
        "joinToString extends Iterable<T>: {joined:?}"
    );
    assert!(
        joined.contains(&"kotlin/IntArray".to_string()),
        "and each specialized array, separately: {joined:?}"
    );

    let iterable = functions
        .iter()
        .find(|function| {
            function.name == "joinToString"
                && function
                    .receiver
                    .as_ref()
                    .is_some_and(|receiver| receiver.render() == "kotlin/collections/Iterable<T>")
        })
        .expect("the Iterable overload");
    assert!(
        iterable
            .params
            .iter()
            .any(|parameter| parameter.render() == "kotlin/Function1<T,kotlin/CharSequence>?"),
        "its transform parameter keeps both its function shape and its nullability: {:?}",
        iterable
            .params
            .iter()
            .map(|parameter| parameter.render())
            .collect::<Vec<_>>()
    );
    assert!(
        iterable.param_defaults.iter().any(|default| *default),
        "and the defaults it declares are recorded"
    );
}

/// `IS_EXPECT_CLASS` survives the decode. This is the bit the JVM backend's common-expectation index
/// selects on — the whole reason it opens a klib at all — so it is pinned against a real `expect`
/// annotation header rather than only exercised through that index.
#[test]
fn an_expect_annotation_header_is_marked_as_one() {
    let Some(directory) = krusty::toolchain::kotlinc_lib_dir() else {
        return;
    };
    let path = directory.join("kotlin-stdlib-wasm-js.klib");
    if !path.is_file() {
        return;
    }
    let archive = KlibArchive::open(&path).expect("open the reference wasm stdlib klib");
    let mut js_static = None;
    for fragment in archive.package_fragments() {
        if fragment.package_fqname != "kotlin.js" {
            continue;
        }
        let Some(bytes) = archive.read(&fragment.entry) else {
            continue;
        };
        if let Some(found) = parse_package_fragment(&bytes)
            .classes
            .remove("kotlin/js/JsStatic")
        {
            js_static = Some(found);
            break;
        }
    }
    let js_static = js_static.expect("JsStatic declaration, from metadata rather than a hardcode");
    assert_eq!(js_static.kind, krusty::libraries::TypeKind::Annotation);
    assert_eq!(js_static.visibility, krusty::types::Visibility::Public);
    assert!(
        js_static.is_expect,
        "JsStatic is a common expect declaration"
    );
    assert!(
        js_static.constructors.iter().any(|constructor| {
            constructor.params.is_empty()
                && constructor.visibility == krusty::types::Visibility::Public
        }),
        "with the public no-argument constructor an annotation use needs"
    );
}

/// A klib the TEST wrote, rather than the one the distribution ships.
///
/// This is the round trip the reader exists to serve: Kotlin source in, a real klib out of the
/// reference compiler, and declarations back — each one matching what the fixture declared. A test
/// can now bring its own library in Kotlin's non-JVM format, the way it has always been able to
/// bring its own JVM dependency.
#[test]
fn a_test_authored_klib_reports_what_its_source_declared() {
    let Some(path) = common::kotlinc_klib(
        "fixture",
        &[(
            "Lib.kt",
            "package fixture\n\
             \n\
             interface Named {\n\
             \x20   val name: String\n\
             }\n\
             \n\
             class Holder<T : Named>(val held: T) : Named {\n\
             \x20   override val name: String get() = held.name\n\
             \x20   fun take(other: T?, count: Int = 1): List<T> = listOf(held)\n\
             }\n\
             \n\
             fun <T : Named> hold(value: T): Holder<T> = Holder(value)\n\
             \n\
             inline fun <T : Named> Holder<T>.describe(render: (T) -> String): String =\n\
             \x20   render(held)\n",
        )],
    ) else {
        return;
    };
    let archive = KlibArchive::open(&path).expect("open the klib the test just built");
    assert_eq!(
        archive.manifest().unique_name(),
        Some("fixture"),
        "the library identity is the one the test chose"
    );

    let (classes, functions) = declarations_of(&archive);
    let holder = classes
        .get("fixture/Holder")
        .expect("the declared class, under its own package");
    assert_eq!(holder.kind, krusty::libraries::TypeKind::Class);
    assert_eq!(holder.supertypes, vec!["fixture/Named"]);
    assert_eq!(
        holder
            .type_params
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>(),
        vec!["T"]
    );
    assert_eq!(
        holder
            .type_params
            .first()
            .expect("one type parameter")
            .bounds
            .iter()
            .map(|bound| bound.render())
            .collect::<Vec<_>>(),
        vec!["fixture/Named"],
        "the declared upper bound, not an inferred Any?"
    );

    let take = holder
        .members
        .iter()
        .find(|member| member.name == "take")
        .expect("Holder.take");
    assert_eq!(
        take.params
            .iter()
            .map(|parameter| parameter.render())
            .collect::<Vec<_>>(),
        vec!["T?", "kotlin/Int"],
        "a nullable type-parameter parameter stays both"
    );
    assert_eq!(take.ret.render(), "kotlin/collections/List<T>");

    let hold = functions
        .iter()
        .find(|function| function.name == "hold")
        .expect("the top-level hold");
    assert_eq!(hold.ret.render(), "fixture/Holder<T>");
    assert!(hold.receiver.is_none(), "hold is not an extension");

    let describe = functions
        .iter()
        .find(|function| function.name == "describe")
        .expect("the extension");
    assert!(describe.is_inline, "declared inline");
    assert_eq!(
        describe.receiver.as_ref().map(|receiver| receiver.render()),
        Some("fixture/Holder<T>".to_string()),
        "and its receiver is the extended type, type argument included"
    );
}
