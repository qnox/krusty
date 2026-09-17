//! Properties a KLIB declares — `val`, `var`, `const`, and extension properties.
//!
//! A klib names no accessor: whether a read is a field load or a getter call is the realizing
//! target's decision. What the klib does carry is the declaration — the type, whether there is a
//! setter, whether the value is a compile-time constant, and an extension property's receiver — and
//! those are what a resolver needs. Each one is asserted against a klib the reference compiler
//! wrote, so the flag word is read off an artifact rather than assumed.

use super::common;
use krusty::klib_symbols::KlibSymbols;
use krusty::libraries::{Callables, PropKind, PropertyInfo};
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::{type_name, Ty};

const LIB: &str = r#"
package plib

class Holder {
    val readOnly: Int = 1
    var mutable: String = ""
    val Int.memberExt: String get() = ""
    fun Int.memberExtFun(): Int = 1
}

const val topConst: Int = 7
val topVal: Int = 1
var topVar: String = ""
val Int.topExt: String get() = ""
"#;

fn symbols() -> Option<KlibSymbols> {
    let klib = common::kotlinc_klib("klib_properties", &[("Lib.kt", LIB)])?;
    Some(KlibSymbols::open(&[klib]))
}

fn properties(callables: &Callables) -> Vec<PropertyInfo> {
    match callables {
        Callables::Properties(properties) | Callables::Both { properties, .. } => {
            properties.overloads.clone()
        }
        _ => Vec::new(),
    }
}

fn only(callables: &Callables, what: &str) -> PropertyInfo {
    let mut overloads = properties(callables);
    assert_eq!(overloads.len(), 1, "one {what}");
    overloads.pop().expect("one overload")
}

#[test]
fn a_member_property_carries_its_type_and_whether_it_has_a_setter() {
    let Some(symbols) = symbols() else {
        return;
    };
    let holder = symbols
        .classifier(type_name("plib/Holder"))
        .expect("plib.Holder");

    let read_only = only(
        holder
            .declared_callables
            .get("readOnly")
            .expect("Holder declares readOnly"),
        "readOnly",
    );
    assert_eq!(read_only.kind, PropKind::Member);
    assert_eq!(read_only.ty, Ty::Int);
    assert!(read_only.receiver.is_none());
    assert!(
        read_only.setter.is_none(),
        "a val has none, and a write against it must be rejected"
    );
    assert!(!read_only.is_const, "it is not a `const val`");

    let mutable = only(
        holder
            .declared_callables
            .get("mutable")
            .expect("Holder declares mutable"),
        "mutable",
    );
    assert_eq!(mutable.ty, Ty::String);
    let setter = mutable.setter.expect("a var declares a setter");
    assert_eq!(
        setter.params.len(),
        1,
        "which takes the new value: {:?}",
        setter.params
    );
    assert!(
        setter.descriptor.is_empty(),
        "and carries no descriptor — how the write is realized is the target's"
    );
}

#[test]
fn a_member_extension_property_keeps_its_receiver() {
    let Some(symbols) = symbols() else {
        return;
    };
    let holder = symbols
        .classifier(type_name("plib/Holder"))
        .expect("plib.Holder");
    let extension = only(
        holder
            .declared_callables
            .get("memberExt")
            .expect("Holder declares memberExt"),
        "memberExt",
    );
    assert_eq!(
        extension.kind,
        PropKind::MemberExtension,
        "its declaring class is the dispatch receiver and Int is the extension receiver"
    );
    assert_eq!(extension.receiver, Some(Ty::Int));
    assert_eq!(
        extension.getter.params,
        vec![Ty::Int],
        "the receiver leads the accessor's parameters"
    );
}

/// A member extension FUNCTION takes the same shape: the receiver at the head of its realized
/// parameters, and its declared signature carrying it separately.
#[test]
fn a_member_extension_function_keeps_its_receiver() {
    let Some(symbols) = symbols() else {
        return;
    };
    let holder = symbols
        .classifier(type_name("plib/Holder"))
        .expect("plib.Holder");
    let functions = match holder
        .declared_callables
        .get("memberExtFun")
        .expect("Holder declares memberExtFun")
    {
        Callables::Functions(functions) | Callables::Both { functions, .. } => {
            functions.overloads.clone()
        }
        _ => panic!("memberExtFun is a function"),
    };
    let overload = functions.first().expect("one overload");
    assert_eq!(overload.callable.params, vec![Ty::Int]);
    assert_eq!(
        overload
            .generic_sig
            .as_ref()
            .expect("a declared signature")
            .receiver,
        Some(Ty::Int),
        "and the declared signature names it as the receiver, not a value parameter"
    );
}

#[test]
fn a_top_level_property_answers_on_its_package() {
    let Some(symbols) = symbols() else {
        return;
    };
    let package = type_name("plib");
    let top_const = only(
        &symbols
            .symbols(SymbolNamespace::Package(package), "topConst")
            .callables,
        "topConst",
    );
    assert_eq!(top_const.kind, PropKind::TopLevel);
    assert!(
        top_const.is_const,
        "a `const val` says so in its flag word, and a use site inlines the value"
    );

    let top_val = only(
        &symbols
            .symbols(SymbolNamespace::Package(package), "topVal")
            .callables,
        "topVal",
    );
    assert!(!top_val.is_const && top_val.setter.is_none());

    let top_var = only(
        &symbols
            .symbols(SymbolNamespace::Package(package), "topVar")
            .callables,
        "topVar",
    );
    assert!(top_var.setter.is_some(), "a top-level var has a setter");

    let top_ext = only(
        &symbols
            .symbols(SymbolNamespace::Package(package), "topExt")
            .callables,
        "topExt",
    );
    assert_eq!(top_ext.kind, PropKind::Extension);
    assert_eq!(top_ext.receiver, Some(Ty::Int));
}

/// Through the driver: every reference to a klib's properties resolves, and the compile then stops
/// where klib ingestion currently ends.
///
/// The record assertions above say the declarations are shaped right; this says resolution finds
/// them. A read of a `val`, a write to a `var`, a `const val` and an extension property each go
/// through a different path, so each is named — and without the flag each one is an unresolved
/// reference, which is what makes their absence with it mean something.
///
/// Past resolution the compile stops, and that is the right answer for this target: a klib callable
/// has no JVM descriptor and no external-callable identity, so the checked FIR cannot name a call
/// to it. A `kotlin.native` declaration has no JVM class to call. Realization is a backend's, and a
/// klib-consuming backend is what will change this assertion.
#[test]
fn source_resolves_against_a_klib_s_properties() {
    let Some(klib) = common::kotlinc_klib("klib_properties", &[("Lib.kt", LIB)]) else {
        return;
    };
    let source = "import plib.topConst\n\
                  import plib.topVar\n\
                  import plib.topExt\n\
                  fun read(h: plib.Holder): Int = h.readOnly\n\
                  fun write(h: plib.Holder) { h.mutable = \"x\" }\n\
                  fun constant(): Int = topConst\n\
                  fun topWrite() { topVar = \"x\" }\n\
                  fun extension(): String = 1.topExt\n";

    // `h.mutable` is not named here: its receiver's own type is unresolved without the klib, so the
    // write is never reached. Every reference the compiler does reach is named.
    let without = compile_source(source, None);
    for name in ["plib", "readOnly", "topConst", "topVar", "topExt"] {
        assert!(
            without.contains(&format!("unresolved reference '{name}'")),
            "without -libraries, {name} resolves to nothing:\n{without}"
        );
    }

    let with = compile_source(source, Some(&klib));
    assert!(
        !with.contains("unresolved reference"),
        "with it, every one of them resolves:\n{with}"
    );
    assert!(
        with.contains("MissingStableCallTarget"),
        "and the compile stops at realization, not resolution — a klib callable has no JVM \
         identity to emit a call to:\n{with}"
    );
}

/// Compile one source against the stdlib jar, optionally with a klib on the dependency path, and
/// return everything the driver said.
fn compile_source(source: &str, libraries: Option<&std::path::Path>) -> String {
    let dir = common::scratch_dir().expect("scratch dir");
    let src = dir.join("Main.kt");
    std::fs::write(&src, source).unwrap();
    let mut command = std::process::Command::new(common::krusty_binary());
    command.args(["-no-stdlib", "-no-jdk", "-cp"]);
    command.arg(common::stdlib_jar());
    if let Some(libraries) = libraries {
        command.arg("-libraries").arg(libraries);
    }
    let out = command
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    report
}
