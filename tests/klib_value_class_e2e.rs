//! `value class` and `fun interface`, as a KLIB declares them.
//!
//! Two declaration bits with consequences a use site can feel. A value class's underlying type is
//! what its representation is built from — core reads the presence of one as "this is a value
//! class" — and a Kotlin interface may be SAM-converted only when it was declared `fun interface`.
//! Neither was read, so a klib's `UInt` looked like an ordinary class and its `Comparator` refused
//! a lambda.

use super::common;
use krusty::klib_symbols::KlibSymbols;
use krusty::symbol_source::SymbolSource;
use krusty::types::{type_name, Ty};

const LIB: &str = r#"
package plib

value class Meters(val raw: Int)

value class Wrapped(val text: String)

fun interface Handler {
    fun handle(x: Int): Int
}

interface Plain {
    fun only(x: Int): Int
}

class Ordinary(val raw: Int)
"#;

fn symbols() -> Option<KlibSymbols> {
    let klib = common::kotlinc_klib("klib_value_class", &[("Lib.kt", LIB)])?;
    Some(KlibSymbols::open(&[klib]))
}

#[test]
fn a_value_class_names_its_underlying_property_and_its_declared_type() {
    let Some(symbols) = symbols() else {
        return;
    };
    for (name, property, underlying) in [
        ("plib/Meters", "raw", Ty::Int),
        ("plib/Wrapped", "text", Ty::String),
    ] {
        let classifier = symbols
            .classifier(type_name(name))
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(
            classifier.value_underlying_property.as_deref(),
            Some(property),
            "{name}"
        );
        assert_eq!(
            classifier.value_underlying,
            Some(underlying),
            "{name} — the DECLARED underlying type; erasing it is a target's business"
        );
    }

    let ordinary = symbols
        .classifier(type_name("plib/Ordinary"))
        .expect("plib.Ordinary");
    assert!(
        ordinary.value_underlying.is_none() && ordinary.value_underlying_property.is_none(),
        "a class with one property is not thereby a value class"
    );
}

#[test]
fn only_a_fun_interface_is_sam_eligible() {
    let Some(symbols) = symbols() else {
        return;
    };
    assert!(
        symbols
            .classifier(type_name("plib/Handler"))
            .expect("plib.Handler")
            .sam_eligible,
        "declared `fun interface`"
    );
    assert!(
        !symbols
            .classifier(type_name("plib/Plain"))
            .expect("plib.Plain")
            .sam_eligible,
        "a single abstract method is not enough for a KOTLIN interface"
    );
}

/// Against the library that ships them: 17 value classes and 3 `fun interface`s, of which these are
/// the ones a Kotlin program meets first.
#[test]
fn the_native_stdlib_s_value_classes_and_fun_interfaces() {
    let Some(stdlib) = krusty::toolchain::kotlin_native_stdlib() else {
        return;
    };
    let symbols = KlibSymbols::open(&[stdlib]);
    for (name, property, underlying) in [
        ("kotlin/UInt", "data", Ty::Int),
        ("kotlin/ULong", "data", Ty::Long),
        ("kotlin/time/Duration", "rawValue", Ty::Long),
    ] {
        let classifier = symbols
            .classifier(type_name(name))
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(
            classifier.value_underlying_property.as_deref(),
            Some(property),
            "{name}"
        );
        assert_eq!(classifier.value_underlying, Some(underlying), "{name}");
    }
    assert!(
        symbols
            .classifier(type_name("kotlin/Comparator"))
            .expect("kotlin.Comparator")
            .sam_eligible,
        "kotlin.Comparator is a `fun interface`, which is what lets a lambda stand in for it"
    );
}
