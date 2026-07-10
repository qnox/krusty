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
        "class Holder(val label: String) { var count: Int = 0 }\nval Holder.tag: String get() = \"t\"",
    ) else {
        eprintln!("skip: kotlinc unavailable");
        return;
    };
    let cp = Rc::new(Classpath::new(vec![dir, stdlib]));
    let lib = JvmLibraries::new(cp.clone());

    // `val label` — a getter, no setter; the getter name comes from metadata, not a `get`+cap guess.
    let props = lib.property_members(Ty::obj("Holder"), "label");
    let label = props
        .overloads
        .iter()
        .find(|p| p.owner == "Holder")
        .expect("label property resolved from @Metadata");
    assert_eq!(label.getter.name, "getLabel");
    assert!(label.setter.is_none(), "a `val` exposes no setter");

    // `var count` — both accessors present.
    let props = lib.property_members(Ty::obj("Holder"), "count");
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
        .property_members(Ty::obj("Holder"), "nope")
        .overloads
        .is_empty());

    // `val Holder.tag` — an EXTENSION property. Extension/top-level declarations are surfaced by the
    // receiver-AGNOSTIC `resolve_symbols` fqn seam (member properties by `property_members`); its getter
    // (a static `getTag(Holder)` on the facade) carries the extension receiver from the Package metadata.
    let props = match lib.resolve_symbols("tag").callables {
        krusty::libraries::Callables::Properties(p) => p.overloads,
        _ => Vec::new(),
    };
    let tag = props
        .iter()
        .find(|p| p.kind == krusty::libraries::PropKind::Extension)
        .expect("extension property tag resolved from @Metadata");
    assert_eq!(tag.getter.name, "getTag");
}

#[test]
fn jvmname_extension_property_resolves_via_metadata_getter() {
    // A classpath extension property whose getter is `@JvmName`-renamed: the `getX` guess (`getTag`)
    // misses the real `grabTag`, so this was `unresolved member 'tag'` before the properties() query
    // supplied the metadata getter name. Compiles AND runs in krusty end-to-end.
    let lib = "package lib\nclass Holder(val label: String)\n\
               val Holder.tag: String @JvmName(\"grabTag\") get() = \"T:\" + label";
    let main = "import lib.Holder\nimport lib.tag\nfun box(): String = Holder(\"x\").tag";
    if let Some(out) = common::run_box_against("jvmnameextprop", lib, main) {
        assert_eq!(
            out, "T:x",
            "@JvmName extension property must resolve via its metadata getter"
        );
    }
}

#[test]
fn classpath_var_member_setter_assigns_via_metadata() {
    // Assigning a classpath `var` member (`b.count = 7`) resolves the property's setter from @Metadata.
    // Before the properties() write path, this was `unresolved member 'count'` — the checker only knew
    // USER-declared props, never the classpath. Compiles AND runs end-to-end.
    let lib = "package lib\nclass Box(var count: Int)";
    let main = "import lib.Box\nfun box(): String {\n  val b = Box(1)\n  b.count = 7\n  \
                return if (b.count == 7) \"OK\" else \"f:${b.count}\"\n}";
    if let Some(out) = common::run_box_against("varsetterplain", lib, main) {
        assert_eq!(
            out, "OK",
            "a classpath var member setter must resolve via its metadata setter"
        );
    }
}

#[test]
fn classpath_jvmname_var_setter_assigns_via_metadata() {
    // A classpath `var` whose accessors are `@JvmName`-renamed: the `setX` guess (`setN`) misses the real
    // `stash`, so the assignment `b.n = 7` needs the metadata setter name from the properties() query.
    let lib = "package lib\nclass Box(var raw: Int) {\n  var n: Int\n    \
               @JvmName(\"grab\") get() = raw\n    @JvmName(\"stash\") set(v) { raw = v }\n}";
    let main = "import lib.Box\nfun box(): String {\n  val b = Box(1)\n  b.n = 7\n  \
                return if (b.n == 7) \"OK\" else \"f:${b.n}\"\n}";
    if let Some(out) = common::run_box_against("varsetterjvmname", lib, main) {
        assert_eq!(
            out, "OK",
            "an @JvmName var setter must resolve via its metadata setter"
        );
    }
}
