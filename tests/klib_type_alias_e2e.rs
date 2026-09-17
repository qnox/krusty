//! Top-level `typealias` declarations a KLIB exports.
//!
//! The Kotlin/Native stdlib ships 37 of them. Each one was a name the library exports that resolved
//! to nothing: the reader did not read `Package.typeAlias` at all, so an alias had no identity, no
//! target and no expansion.

use super::common;
use krusty::klib_symbols::KlibSymbols;
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::{type_name, Ty};

const LIB: &str = r#"
package plib

class PBox<A, B>(val a: A, val b: B)

typealias Plain = PBox<Int, String>
typealias Boxed<T> = PBox<T, T>
typealias Chain = Boxed<Int>
"#;

fn symbols() -> Option<KlibSymbols> {
    let klib = common::kotlinc_klib("klib_type_alias", &[("Lib.kt", LIB)])?;
    Some(KlibSymbols::open(&[klib]))
}

/// A plain alias names its target and expands to it.
#[test]
fn an_alias_resolves_to_its_target() {
    let Some(symbols) = symbols() else {
        return;
    };
    let record = symbols.symbols(SymbolNamespace::Package(type_name("plib")), "Plain");
    assert_eq!(
        record.classifier_name,
        Some(type_name("plib/PBox")),
        "the alias resolves to its TARGET's identity"
    );
    let classifier = record.classifier.as_ref().expect("the target's record");
    assert_eq!(
        classifier.alias_target,
        Some(type_name("plib/PBox")),
        "tagged with the target it came through"
    );
    assert!(
        record.importable_declaration,
        "and an alias is importable in its own right"
    );

    let expansion = symbols
        .type_alias_expansion(type_name("plib/Plain"))
        .expect("plib.Plain has a template");
    assert_eq!(expansion.identity, type_name("plib/Plain"));
    assert_eq!(expansion.target, type_name("plib/PBox"));
    assert!(expansion.formals.is_empty(), "Plain takes no parameters");
    assert_eq!(
        expansion.expansion.type_args(),
        [Ty::Int, Ty::String],
        "and expands to the target applied to ITS arguments: {:?}",
        expansion.expansion
    );
}

/// A generic alias's parameters are the substitution domain, and one parameter may land in several
/// positions of the target — which is why a use site substitutes rather than pasting.
#[test]
fn a_generic_alias_maps_its_parameters_onto_the_target() {
    let Some(symbols) = symbols() else {
        return;
    };
    let expansion = symbols
        .type_alias_expansion(type_name("plib/Boxed"))
        .expect("plib.Boxed has a template");
    assert_eq!(expansion.formals, vec!["T".to_string()]);
    let args = expansion.expansion.type_args();
    assert_eq!(
        args.len(),
        2,
        "one alias parameter, two target positions: {:?}",
        expansion.expansion
    );
    assert!(
        args.iter().all(|arg| matches!(arg, Ty::TyParam(..))),
        "both of them the alias's own parameter, still symbolic: {args:?}"
    );
}

/// An alias whose right-hand side names another alias records the EXPANDED type, not the spelling.
#[test]
fn a_chained_alias_records_the_expansion() {
    let Some(symbols) = symbols() else {
        return;
    };
    let expansion = symbols
        .type_alias_expansion(type_name("plib/Chain"))
        .expect("plib.Chain has a template");
    assert_eq!(
        expansion.target,
        type_name("plib/PBox"),
        "Chain = Boxed<Int> expands past Boxed to PBox"
    );
    assert_eq!(
        expansion.expansion.type_args(),
        [Ty::Int, Ty::Int],
        "with Boxed's parameter already substituted: {:?}",
        expansion.expansion
    );
    assert_eq!(
        expansion.expansion_spelling,
        krusty::spelling::Spelled::default(),
        "the `Boxed<Int>` abbreviation lives in Type.abbreviatedTypeId, which is not read yet"
    );
}

/// Against the library that actually ships them: 37 aliases, of which these are the familiar ones.
/// `LinkedHashMap` is a stdlib name every Kotlin program can write, and on Native it is an alias.
#[test]
fn the_native_stdlib_s_aliases_name_their_targets() {
    let Some(stdlib) = krusty::toolchain::kotlin_native_stdlib() else {
        return;
    };
    let symbols = KlibSymbols::open(&[stdlib]);
    for (alias, target, formals) in [
        (
            "kotlin/collections/LinkedHashMap",
            "kotlin/collections/HashMap",
            2,
        ),
        (
            "kotlin/collections/LinkedHashSet",
            "kotlin/collections/HashSet",
            1,
        ),
        ("kotlin/native/Throws", "kotlin/Throws", 0),
        ("kotlinx/cinterop/IntVar", "kotlinx/cinterop/IntVarOf", 0),
    ] {
        let expansion = symbols
            .type_alias_expansion(type_name(alias))
            .unwrap_or_else(|| panic!("{alias} is one of the stdlib's aliases"));
        assert_eq!(expansion.target, type_name(target), "{alias}");
        assert_eq!(expansion.formals.len(), formals, "{alias}");
        assert!(
            symbols.classifier(expansion.target).is_some(),
            "{alias}'s target is itself a declared classifier"
        );
    }

    // And the alias resolves by name, through the namespace a use site walks.
    let record = symbols.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "LinkedHashMap",
    );
    assert_eq!(
        record.classifier_name,
        Some(type_name("kotlin/collections/HashMap"))
    );
    assert!(record.importable_declaration);
}

/// Through the driver: a source naming a klib's alias resolves and, being only a type reference,
/// compiles all the way out — the alias expands to a classifier the target can name.
#[test]
fn source_compiles_against_a_klib_s_alias() {
    let Some(klib) = common::kotlinc_klib("klib_type_alias", &[("Lib.kt", LIB)]) else {
        return;
    };
    let dir = common::scratch_dir().expect("scratch dir");
    let src = dir.join("Main.kt");
    std::fs::write(&src, "fun keep(boxed: plib.Plain) = boxed\n").unwrap();
    let mut command = std::process::Command::new(common::krusty_binary());
    command.args(["-no-stdlib", "-no-jdk", "-cp"]);
    command.arg(common::stdlib_jar());
    let out = command
        .arg("-libraries")
        .arg(&klib)
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(
        !report.contains("unresolved reference"),
        "plib.Plain resolves through the alias:\n{report}"
    );
    assert!(
        out.status.success(),
        "and a type reference needs no klib callable to realize:\n{report}"
    );
}
