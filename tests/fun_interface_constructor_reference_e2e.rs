//! A `fun interface` constructor reference is a `FunInterfaceConstructorReference`, so it is a
//! `KFunction`, stays equal across files and type arguments, and still builds the SAM wrapper.
//!
//! krusty and kotlinc compile the same sources. `box()` results match, and every carrier whose
//! superclass is `FunInterfaceConstructorReference` matches as a complete set: superclass, interfaces
//! in class-file order, and each `<init>` / `invoke` name, descriptor, and access flags.

use std::path::Path;

use super::common;

const CARRIER_SUPER: &str = "kotlin/jvm/internal/FunInterfaceConstructorReference";

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct InvocationMember {
    name: String,
    descriptor: String,
    access: u16,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CarrierAbi {
    superclass: String,
    interfaces: Vec<String>,
    members: Vec<InvocationMember>,
}

fn class_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("class directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().is_some_and(|ext| ext == "class") {
                let name = path
                    .strip_prefix(root)
                    .expect("class path")
                    .to_string_lossy()
                    .trim_end_matches(".class")
                    .replace('\\', "/");
                out.push((name, std::fs::read(&path).expect("class bytes")));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

fn carrier_abis(classes: &[(String, Vec<u8>)]) -> Vec<CarrierAbi> {
    let mut carriers = Vec::new();
    for (name, bytes) in classes {
        let info = krusty::jvm::classreader::parse_class(bytes)
            .unwrap_or_else(|error| panic!("parse {name}: {error:?}"));
        let Some(superclass) = info.super_class else {
            continue;
        };
        if !superclass.matches(CARRIER_SUPER) {
            continue;
        }
        let mut members = info
            .methods
            .iter()
            .filter(|method| method.name == "<init>" || method.name == "invoke")
            .map(|method| InvocationMember {
                name: method.name.clone(),
                descriptor: method.descriptor.clone(),
                access: method.access,
            })
            .collect::<Vec<_>>();
        members.sort();
        carriers.push(CarrierAbi {
            superclass: superclass.render(),
            interfaces: info.interfaces(),
            members,
        });
    }
    carriers.sort();
    carriers
}

fn compile_krusty(sources: &[(&str, &str)], label: &str) -> Vec<(String, Vec<u8>)> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_in_process_files(sources, &[stdlib], Some(jdk.as_path())).unwrap_or_else(|| {
        let files = sources
            .iter()
            .map(|(_, source)| *source)
            .collect::<Vec<_>>();
        let diagnostics = common::front_end_diagnostics_files_with_stdlib(&files);
        panic!("{label}: krusty rejected the sources: {diagnostics:?}");
    })
}

fn compile_kotlinc(sources: &[(&str, &str)], label: &str) -> Vec<(String, Vec<u8>)> {
    let dir = common::kotlinc_lib_out(sources)
        .unwrap_or_else(|| panic!("{label}: kotlinc is unavailable"));
    class_bytes(&dir)
}

fn assert_same_program(sources: &[(&str, &str)], label: &str) {
    let krusty = compile_krusty(sources, label);
    let kotlinc = compile_kotlinc(sources, label);
    let krusty_carriers = carrier_abis(&krusty);
    let kotlinc_carriers = carrier_abis(&kotlinc);
    assert!(
        !kotlinc_carriers.is_empty(),
        "{label}: kotlinc emitted no FunInterfaceConstructorReference carrier"
    );
    assert_eq!(
        krusty_carriers, kotlinc_carriers,
        "{label}: fun-interface constructor carriers"
    );

    let stdlib = common::stdlib_jar();
    let krusty_box = common::find_box_class(&krusty)
        .unwrap_or_else(|| panic!("{label}: krusty emitted no box()"));
    let kotlinc_box = common::find_box_class(&kotlinc)
        .unwrap_or_else(|| panic!("{label}: kotlinc emitted no box()"));
    let krusty_out = common::run_box(&krusty, &krusty_box, std::slice::from_ref(&stdlib))
        .unwrap_or_else(|| panic!("{label}: krusty box() did not run"));
    let kotlinc_out = common::run_box(&kotlinc, &kotlinc_box, std::slice::from_ref(&stdlib))
        .unwrap_or_else(|| panic!("{label}: kotlinc box() did not run"));
    assert_eq!(krusty_out, kotlinc_out, "{label}: box()");
    assert_eq!(kotlinc_out, "OK", "{label}: kotlinc box()");
}

#[test]
fn implicit_kfunction_constructor_reference_invokes() {
    const SOURCE: &str = "fun interface KRunnable {\n\
         fun run()\n\
         }\n\
         val kr = ::KRunnable\n\
         fun box(): String {\n\
             var test = \"Failed\"\n\
             kr { test = \"OK\" }.run()\n\
             return test\n\
         }\n";
    assert_same_program(&[("FunIfaceCtor.kt", SOURCE)], "FunIfaceCtor");
}

#[test]
fn fun_interface_constructor_reference_is_a_kfunction() {
    const SOURCE: &str = "import kotlin.reflect.KFunction\n\
         fun interface KRunnable {\n\
             fun run()\n\
         }\n\
         val kr = ::KRunnable\n\
         fun box(): String = if (kr is KFunction<*>) \"OK\" else \"Fail\"\n";
    assert_same_program(&[("FunIfaceKFunction.kt", SOURCE)], "FunIfaceKFunction");
}

#[test]
fn fun_interface_constructor_references_compare_by_interface_class() {
    const FIRST: &str = "fun interface KRunnable { fun run() }\n\
         val ks1: (() -> String) -> KSupplier<String> = ::KSupplier\n\
         val ks2: (() -> String) -> KSupplier<String> = ::KSupplier\n\
         val kn1: (() -> Number) -> KSupplier<Number> = ::KSupplier\n\
         fun checkEqual(message: String, a1: Any, a2: Any) {\n\
             if (a1 != a2) throw Exception(message)\n\
             if (a1.hashCode() != a2.hashCode()) throw Exception(message)\n\
         }\n\
         fun checkNotEqual(message: String, a1: Any, a2: Any) {\n\
             if (a1 == a2) throw Exception(message)\n\
         }\n\
         fun box(): String {\n\
             checkEqual(\"same file\", ks1, ks2)\n\
             checkEqual(\"other file\", ks1, ks3)\n\
             checkEqual(\"type arguments\", ks1, kn1)\n\
             val kr: (() -> Unit) -> KRunnable = ::KRunnable\n\
             checkNotEqual(\"other interface\", ks1, kr)\n\
             return \"OK\"\n\
         }\n";
    const SECOND: &str = "fun interface KSupplier<T> { fun get(): T }\n\
         val ks3: (() -> String) -> KSupplier<String> = ::KSupplier\n";
    assert_same_program(
        &[
            ("funInterfaceConstructorEquality.kt", FIRST),
            ("KSupplier.kt", SECOND),
        ],
        "FunIfaceEquality",
    );
}
