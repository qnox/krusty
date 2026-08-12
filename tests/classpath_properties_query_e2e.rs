//! P3: `SymbolSource::properties` on a JVM classpath returns a type's member properties from its
//! `@Metadata`, carrying the REAL getter/setter JVM names (from the `JvmPropertySignature`) rather than
//! guessing `getX` — the seam that will replace `resolve_property_member`'s getter-name convention.

use std::rc::Rc;

use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::symbol_source::SymbolSource;
use krusty::types::Ty;

use super::common;

fn declared(lib: &JvmLibraries, receiver: Ty, name: &str) -> krusty::libraries::Callables {
    receiver
        .kotlin_class_internal()
        .and_then(|internal| lib.classifier(internal))
        .and_then(|classifier| classifier.declared_callables.get(name).cloned())
        .unwrap_or_default()
}

#[test]
fn core_inherits_mapped_string_length_property() {
    let lib = JvmLibraries::new(Rc::new(Classpath::new(vec![common::stdlib_jar()])));
    let resolver = krusty::symbol_resolver::SymbolResolver::new(&lib);
    let length = resolver
        .select_member_property(Ty::String, "length")
        .expect("core hierarchy walk must inherit CharSequence.length");
    assert_eq!(length.ty, Ty::Int);
    let applied = resolver
        .select_member_property(Ty::obj("kotlin/String"), "length")
        .expect("interned Kotlin String identity must use the same hierarchy");
    assert_eq!(applied.ty, Ty::Int);
}

#[test]
fn member_property_getter_and_setter_from_metadata() {
    let stdlib = common::stdlib_jar();
    let Some(dir) = common::compile_lib_ref(
        "propquery",
        "class Holder(val label: String) { var count: Int = 0 }\n\
         val Holder.tag: String get() = \"t\"\n\
         fun Holder.tag(): String = \"f\"\n\
         val Holder.isTagged: Boolean get() = true",
    ) else {
        eprintln!("skip: kotlinc unavailable");
        return;
    };
    let cp = Rc::new(Classpath::new(vec![dir, stdlib]));
    let lib = JvmLibraries::new(cp.clone());

    // `val label` — a getter, no setter; the getter name comes from metadata, not a `get`+cap guess.
    let props = declared(&lib, Ty::obj("Holder"), "label").into_parts().1;
    let label = props
        .overloads
        .iter()
        .find(|p| p.owner.matches("Holder"))
        .expect("label property resolved from @Metadata");
    assert_eq!(label.getter.name, "getLabel");
    assert!(label.setter.is_none(), "a `val` exposes no setter");

    // `var count` — both accessors present.
    let props = declared(&lib, Ty::obj("Holder"), "count").into_parts().1;
    let count = props
        .overloads
        .iter()
        .find(|p| p.owner.matches("Holder"))
        .expect("count property resolved from @Metadata");
    assert_eq!(count.getter.name, "getCount");
    assert_eq!(
        count.setter.as_ref().map(|s| s.name.as_str()),
        Some("setCount"),
        "a `var` exposes its setter"
    );

    // An absent name yields nothing.
    assert!(declared(&lib, Ty::obj("Holder"), "nope")
        .into_parts()
        .1
        .overloads
        .is_empty());

    // `val Holder.tag` — an EXTENSION property. Extension/top-level declarations are surfaced by the
    // receiver-agnostic `resolve_symbols` FQN seam; its getter
    // (a static `getTag(Holder)` on the facade) carries the extension receiver from the Package metadata.
    let root = krusty::symbol_source::SymbolNamespace::Package(krusty::types::TypeName::ROOT);
    let symbols = lib.symbols(root, "tag");
    let props = match &symbols.callables {
        krusty::libraries::Callables::Properties(p) => p.overloads.clone(),
        krusty::libraries::Callables::Both { properties, .. } => properties.overloads.clone(),
        _ => Vec::new(),
    };
    let tag = props
        .iter()
        .find(|p| p.kind == krusty::libraries::PropKind::Extension)
        .expect("extension property tag resolved from @Metadata");
    assert_eq!(tag.getter.name, "getTag");
    assert!(matches!(
        symbols.callables,
        krusty::libraries::Callables::Both { .. }
    ));

    let props = match lib.symbols(root, "isTagged").callables.clone() {
        krusty::libraries::Callables::Properties(p) => p.overloads,
        _ => Vec::new(),
    };
    let is_tagged = props
        .iter()
        .find(|p| p.kind == krusty::libraries::PropKind::Extension)
        .expect("is-prefixed extension property resolved from @Metadata");
    assert_eq!(is_tagged.getter.name, "isTagged");
}

#[test]
fn jvmname_extension_property_resolves_via_metadata_getter() {
    // A classpath extension property whose getter is `@JvmName`-renamed: the `getX` guess (`getTag`)
    // misses the real `grabTag`, so this was `unresolved member 'tag'` before the properties() query
    // supplied the metadata getter name. Compiles AND runs in krusty end-to-end.
    let lib = "package lib\nclass Holder(val label: String)\n\
               val Holder.tag: String @JvmName(\"grabTag\") get() = \"T:\" + label";
    let main = "import lib.Holder\nimport lib.tag\nfun box(): String = Holder(\"x\").tag";
    let Some(out) = common::expect_box_run_against("jvmnameextprop", lib, main) else {
        return; // toolchain not provisioned
    };
    assert_eq!(
        out, "T:x",
        "@JvmName extension property must resolve via its metadata getter"
    );
}

#[test]
fn classpath_var_member_setter_assigns_via_metadata() {
    // Assigning a classpath `var` member (`b.count = 7`) resolves the property's setter from @Metadata.
    // Before the properties() write path, this was `unresolved member 'count'` — the checker only knew
    // USER-declared props, never the classpath. Compiles AND runs end-to-end.
    let lib = "package lib\nclass Box(var count: Int)";
    let main = "import lib.Box\nfun box(): String {\n  val b = Box(1)\n  b.count = 7\n  \
                return if (b.count == 7) \"OK\" else \"f:${b.count}\"\n}";
    let Some(out) = common::expect_box_run_against("varsetterplain", lib, main) else {
        return; // toolchain not provisioned
    };
    assert_eq!(
        out, "OK",
        "a classpath var member setter must resolve via its metadata setter"
    );
}

#[test]
fn classpath_jvmname_var_setter_assigns_via_metadata() {
    // A classpath `var` whose accessors are `@JvmName`-renamed: the `setX` guess (`setN`) misses the real
    // `stash`, so the assignment `b.n = 7` needs the metadata setter name from the properties() query.
    let lib = "package lib\nclass Box(var raw: Int) {\n  var n: Int\n    \
               @JvmName(\"grab\") get() = raw\n    @JvmName(\"stash\") set(v) { raw = v }\n}";
    let main = "import lib.Box\nfun box(): String {\n  val b = Box(1)\n  b.n = 7\n  \
                return if (b.n == 7) \"OK\" else \"f:${b.n}\"\n}";
    let Some(out) = common::expect_box_run_against_ref("varsetterjvmname", lib, main) else {
        return; // toolchain not provisioned
    };
    assert_eq!(
        out, "OK",
        "an @JvmName var setter must resolve via its metadata setter"
    );
}

#[test]
fn classpath_var_extension_property_uses_the_selected_metadata_accessors() {
    // Extension-property writes use the same PropertyInfo handoff as reads. In particular, neither
    // the checker nor lowering may reconstruct `getScore`/`setScore`: metadata owns both physical
    // names, and the selected dependency property supplies them together.
    let lib = "package lib\nclass Box(var raw: Int)\n\
               var Box.score: Int\n  @JvmName(\"readScore\") get() = raw\n  \
               @JvmName(\"writeScore\") set(v) { raw = v }";
    let main = "import lib.Box\nimport lib.score\nfun box(): String {\n  val b = Box(1)\n  \
                b.score = 7\n  return if (b.score == 7) \"OK\" else \"f:${b.score}\"\n}";
    let Some(out) = common::expect_box_run_against("varextsetterjvmname", lib, main) else {
        return; // toolchain not provisioned
    };
    assert_eq!(out, "OK");
}
