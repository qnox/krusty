//! P3: `SymbolSource::properties` on a JVM classpath returns a type's member properties from its
//! `@Metadata`, carrying the REAL getter/setter JVM names (from the `JvmPropertySignature`) rather than
//! guessing `getX` — the seam that will replace `resolve_property_member`'s getter-name convention.

use std::rc::Rc;

use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::symbol_source::SymbolSource;
use krusty::types::Ty;

use super::common;

#[test]
fn member_property_getter_and_setter_from_metadata() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let Some(dir) = common::compile_lib(
        "propquery",
        "class Holder(val label: String) { var count: Int = 0 }",
    ) else {
        eprintln!("skip: kotlinc unavailable");
        return;
    };
    let cp = Rc::new(Classpath::new(vec![dir, stdlib]));
    let lib = JvmLibraries::new(cp);

    // `val label` — a getter, no setter; the getter name comes from metadata, not a `get`+cap guess.
    let props = lib.properties("label", Some(Ty::obj("Holder")));
    let label = props
        .overloads
        .iter()
        .find(|p| p.owner == "Holder")
        .expect("label property resolved from @Metadata");
    assert_eq!(label.getter.name, "getLabel");
    assert!(label.setter.is_none(), "a `val` exposes no setter");

    // `var count` — both accessors present.
    let props = lib.properties("count", Some(Ty::obj("Holder")));
    let count = props
        .overloads
        .iter()
        .find(|p| p.owner == "Holder")
        .expect("count property resolved from @Metadata");
    assert_eq!(count.getter.name, "getCount");
    assert_eq!(
        count.setter.as_ref().map(|s| s.name.as_str()),
        Some("setCount"),
        "a `var` exposes its setter"
    );

    // An absent name yields nothing.
    assert!(lib
        .properties("nope", Some(Ty::obj("Holder")))
        .overloads
        .is_empty());
}
