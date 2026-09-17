//! An `enum class`, a `sealed class` and a companion object, as a KLIB records them.
//!
//! These three facts live on the classifier record and nowhere else: the entry names that make
//! `Color.RED` resolve, the direct subclasses that prove an exhaustive `when`, and the companion
//! instance a bare classifier reference in value position denotes. The reader had the companion's
//! name and dropped it; the other two it did not read at all.

use super::common;
use krusty::klib_symbols::KlibSymbols;
use krusty::symbol_source::SymbolSource;
use krusty::types::{type_name, Ty};

const LIB: &str = r#"
package plib

enum class Color { RED, GREEN }

sealed class Shape {
    class Circle : Shape()
    class Square : Shape()
}

class WithCompanion {
    companion object Named {
        val k: Int = 1
    }
}

class Outer {
    inner class Inner
    class Nested
}
"#;

fn symbols() -> Option<KlibSymbols> {
    let klib = common::kotlinc_klib("klib_classifier_shape", &[("Lib.kt", LIB)])?;
    Some(KlibSymbols::open(&[klib]))
}

#[test]
fn an_enum_declares_its_entries_in_order() {
    let Some(symbols) = symbols() else {
        return;
    };
    let color = symbols
        .classifier(type_name("plib/Color"))
        .expect("plib.Color");
    assert_eq!(
        color.enum_entries,
        vec!["RED".to_string(), "GREEN".to_string()],
        "in declaration order, which an entry's ordinal depends on"
    );
}

#[test]
fn a_sealed_class_names_its_direct_subclasses() {
    let Some(symbols) = symbols() else {
        return;
    };
    let shape = symbols
        .classifier(type_name("plib/Shape"))
        .expect("plib.Shape");
    let subclasses = shape
        .sealed_subclasses
        .iter()
        .map(|subclass| subclass.render())
        .collect::<Vec<_>>();
    assert_eq!(
        subclasses,
        vec![
            "plib/Shape$Circle".to_string(),
            "plib/Shape$Square".to_string()
        ],
        "the packed qualified-name ids resolve to the nested identities — rendered in the nested \
         spelling a TypeName uses, not the `.` the fragment writes"
    );
    assert!(
        symbols.classifier(type_name("plib/Shape.Circle")).is_some(),
        "and each subclass is itself a declared classifier, reached by the same identity the \
         fragment's own spelling interns to"
    );
}

#[test]
fn a_companion_object_is_named_relative_to_its_owner() {
    let Some(symbols) = symbols() else {
        return;
    };
    let owner = symbols
        .classifier(type_name("plib/WithCompanion"))
        .expect("plib.WithCompanion");
    assert_eq!(
        owner.companion_object,
        Some(("Named".to_string(), type_name("plib/WithCompanion.Named"))),
        "a named companion keeps its source name, and the field's type is the nested identity"
    );
    let companion = symbols
        .classifier(type_name("plib/WithCompanion.Named"))
        .expect("the companion is a classifier of its own");
    let k = match companion
        .declared_callables
        .get("k")
        .expect("the companion declares k")
    {
        krusty::libraries::Callables::Properties(properties)
        | krusty::libraries::Callables::Both { properties, .. } => {
            properties.overloads.first().expect("one k").clone()
        }
        _ => panic!("k is a property"),
    };
    assert_eq!(k.ty, Ty::Int);
}

/// An `inner` class captures an instance of its enclosing class; a plain nested one does not. The
/// fragment records only the `isInner` bit, because the enclosing class is the nested identity's
/// own owner.
#[test]
fn an_inner_class_captures_its_outer_instance() {
    let Some(symbols) = symbols() else {
        return;
    };
    let inner = symbols
        .classifier(type_name("plib/Outer.Inner"))
        .expect("plib.Outer.Inner");
    assert!(inner.is_nested);
    assert_eq!(
        inner.outer_instance,
        Some(type_name("plib/Outer")),
        "an inner class's outer instance is its owner"
    );

    let nested = symbols
        .classifier(type_name("plib/Outer.Nested"))
        .expect("plib.Outer.Nested");
    assert!(nested.is_nested);
    assert!(
        nested.outer_instance.is_none(),
        "a plain nested class captures nothing"
    );
}
