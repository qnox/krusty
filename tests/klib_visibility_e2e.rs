//! What a KLIB says about who may see a declaration.
//!
//! Visibility was not read for members at all, so every one of them resolved as public. That is not
//! a missing answer but a wrong one: the Kotlin/Native stdlib's own linkdata carries 293 private, 67
//! protected and 84 internal member functions, and reporting them as public lets a source call
//! declarations the library does not expose.
//!
//! The source reports the declared visibility; whether a given call site may reach it is core's
//! access check to make, not this provider's.

use super::common;
use krusty::klib_symbols::KlibSymbols;
use krusty::libraries::Callables;
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::{type_name, Visibility};

const LIB: &str = r#"
package plib

open class Holder {
    private fun hidden(): Int = 1
    internal fun shared(): Int = 2
    protected fun guarded(): Int = 3
    fun exposed(): Int = 4

    private val secret: Int = 5
    internal var shareable: Int = 6
    val visible: Int = 7
}

private fun topHidden(): Int = 1
internal fun topShared(): Int = 2
fun topExposed(): Int = 3
"#;

fn symbols() -> Option<KlibSymbols> {
    let klib = common::kotlinc_klib("klib_visibility", &[("Lib.kt", LIB)])?;
    Some(KlibSymbols::open(&[klib]))
}

fn function_visibility(callables: &Callables) -> Option<Visibility> {
    match callables {
        Callables::Functions(functions) | Callables::Both { functions, .. } => functions
            .overloads
            .first()
            .map(|overload| overload.visibility),
        _ => None,
    }
}

#[test]
fn a_member_reports_the_visibility_it_was_declared_with() {
    let Some(symbols) = symbols() else {
        return;
    };
    let holder = symbols
        .classifier(type_name("plib/Holder"))
        .expect("plib.Holder");
    for (name, expected) in [
        ("hidden", Visibility::Private),
        ("shared", Visibility::Internal),
        ("guarded", Visibility::Protected),
        ("exposed", Visibility::Public),
    ] {
        let callables = holder
            .declared_callables
            .get(name)
            .unwrap_or_else(|| panic!("Holder declares {name}"));
        assert_eq!(
            function_visibility(callables),
            Some(expected),
            "Holder.{name}"
        );
    }
}

#[test]
fn a_property_reports_its_own_visibility_and_its_setter_s() {
    let Some(symbols) = symbols() else {
        return;
    };
    let holder = symbols
        .classifier(type_name("plib/Holder"))
        .expect("plib.Holder");
    let property = |name: &str| match holder
        .declared_callables
        .get(name)
        .unwrap_or_else(|| panic!("Holder declares {name}"))
    {
        Callables::Properties(properties) | Callables::Both { properties, .. } => {
            properties.overloads.first().expect("one overload").clone()
        }
        _ => panic!("{name} is a property"),
    };
    assert_eq!(property("secret").visibility, Visibility::Private);
    assert_eq!(property("visible").visibility, Visibility::Public);

    let shareable = property("shareable");
    assert_eq!(shareable.visibility, Visibility::Internal);
    assert!(shareable.setter.is_some(), "it is a var");
    assert_eq!(
        shareable.setter_visibility,
        Visibility::Internal,
        "a klib records one visibility per property, so the setter's is the property's"
    );
}

#[test]
fn a_top_level_function_reports_its_visibility_too() {
    let Some(symbols) = symbols() else {
        return;
    };
    let package = type_name("plib");
    for (name, expected) in [
        ("topHidden", Visibility::Private),
        ("topShared", Visibility::Internal),
        ("topExposed", Visibility::Public),
    ] {
        let record = symbols.symbols(SymbolNamespace::Package(package), name);
        assert_eq!(
            function_visibility(&record.callables),
            Some(expected),
            "plib.{name}"
        );
    }
}
