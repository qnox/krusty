//! The annotations a KLIB's callables carry.
//!
//! Identities, which is what the member layer records and what decides `@Deprecated`,
//! `@PublishedApi` and the opt-in markers. None of them were read, so every klib declaration looked
//! unannotated — a call to a deprecated stdlib function drew no warning because nothing said it was
//! deprecated. The Kotlin/Native stdlib annotates 952 of its 2964 member functions, 90 of them
//! `@Deprecated`.

use super::common;
use krusty::klib_symbols::KlibSymbols;
use krusty::libraries::Callables;
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::{type_name, TypeName};

const LIB: &str = r#"
package plib

@Target(AnnotationTarget.FUNCTION)
@Retention(AnnotationRetention.BINARY)
annotation class Marker

@Deprecated("use something else")
fun deprecatedTop(): Int = 1

@Marker
fun markedTop(): Int = 2

fun plainTop(): Int = 3

class Holder {
    @Deprecated("gone")
    fun deprecatedMember(): Int = 4

    @Marker
    fun markedMember(): Int = 5

    fun plainMember(): Int = 6
}
"#;

fn symbols() -> Option<KlibSymbols> {
    let klib = common::kotlinc_klib("klib_annotations", &[("Lib.kt", LIB)])?;
    Some(KlibSymbols::open(&[klib]))
}

fn annotations(callables: &Callables, name: &str) -> Vec<TypeName> {
    match callables {
        Callables::Functions(functions) | Callables::Both { functions, .. } => functions
            .overloads
            .first()
            .map(|overload| overload.annotations.clone())
            .unwrap_or_default(),
        _ => panic!("{name} is a function"),
    }
}

#[test]
fn a_member_carries_the_annotations_it_was_declared_with() {
    let Some(symbols) = symbols() else {
        return;
    };
    let holder = symbols
        .classifier(type_name("plib/Holder"))
        .expect("plib.Holder");
    let of = |name: &str| {
        annotations(
            holder
                .declared_callables
                .get(name)
                .unwrap_or_else(|| panic!("Holder declares {name}")),
            name,
        )
    };
    assert_eq!(of("deprecatedMember"), vec![type_name("kotlin/Deprecated")]);
    assert_eq!(
        of("markedMember"),
        vec![type_name("plib/Marker")],
        "a library's own annotation is named by its own identity"
    );
    assert!(
        of("plainMember").is_empty(),
        "and an unannotated member carries none"
    );
}

#[test]
fn a_top_level_function_carries_them_too() {
    let Some(symbols) = symbols() else {
        return;
    };
    let package = type_name("plib");
    let of = |name: &str| {
        annotations(
            &symbols
                .symbols(SymbolNamespace::Package(package), name)
                .callables,
            name,
        )
    };
    assert_eq!(of("deprecatedTop"), vec![type_name("kotlin/Deprecated")]);
    assert_eq!(of("markedTop"), vec![type_name("plib/Marker")]);
    assert!(of("plainTop").is_empty());
}

/// Against the library that ships them.
#[test]
fn the_native_stdlib_s_members_carry_their_annotations() {
    let Some(stdlib) = krusty::toolchain::kotlin_native_stdlib() else {
        return;
    };
    let symbols = KlibSymbols::open(&[stdlib]);
    for (owner, member, annotation) in [
        ("kotlin/text/Regex", "matchesAt", "kotlin/SinceKotlin"),
        (
            "kotlin/text/StringBuilder",
            "reverse",
            "kotlin/IgnorableReturnValue",
        ),
    ] {
        let classifier = symbols
            .classifier(type_name(owner))
            .unwrap_or_else(|| panic!("{owner}"));
        let callables = classifier
            .declared_callables
            .get(member)
            .unwrap_or_else(|| panic!("{owner}.{member}"));
        let carried = match callables {
            Callables::Functions(functions) | Callables::Both { functions, .. } => functions
                .overloads
                .iter()
                .flat_map(|overload| overload.annotations.clone())
                .collect::<Vec<_>>(),
            _ => panic!("{owner}.{member} is a function"),
        };
        assert!(
            carried.contains(&type_name(annotation)),
            "{owner}.{member} carries {annotation}: {:?}",
            carried.iter().map(|a| a.render()).collect::<Vec<_>>()
        );
    }
}
