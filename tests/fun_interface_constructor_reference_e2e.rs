//! A `fun interface` constructor reference is a `FunInterfaceConstructorReference`, so it is a
//! `KFunction`, stays equal across files and type arguments, and still builds the SAM wrapper.
//!
//! krusty and kotlinc compile the same sources. `box()` results match. Each carrier is keyed by
//! its internal class name. That record is the class flags, superclass, interfaces in class-file
//! order, and every field and method name, descriptor, and access flags.

use std::collections::BTreeMap;
use std::path::Path;

use super::common;

const CARRIER_SUPER: &str = "kotlin/jvm/internal/FunInterfaceConstructorReference";

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemberAbi {
    name: String,
    descriptor: String,
    access: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CarrierAbi {
    access: u16,
    superclass: String,
    interfaces: Vec<String>,
    fields: Vec<MemberAbi>,
    methods: Vec<MemberAbi>,
}

fn is_kotlin_module(name: &str) -> bool {
    name.rsplit('/')
        .next()
        .is_some_and(|file| file.ends_with(".kotlin_module"))
}

/// Class artifacts selected for comparison. The Kotlin module resource is not a class file.
fn selected_class_artifacts(artifacts: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    artifacts
        .into_iter()
        .filter(|(name, _)| !is_kotlin_module(name))
        .collect()
}

fn class_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|error| panic!("read entry in {}: {error}", dir.display()));
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().is_some_and(|ext| ext == "class") {
                let name = path
                    .strip_prefix(root)
                    .unwrap_or_else(|error| panic!("class path {}: {error}", path.display()))
                    .to_string_lossy()
                    .trim_end_matches(".class")
                    .replace('\\', "/");
                let bytes = std::fs::read(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                out.push((name, bytes));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

fn member_abi(name: &str, descriptor: &str, access: u16) -> MemberAbi {
    MemberAbi {
        name: name.to_string(),
        descriptor: descriptor.to_string(),
        access,
    }
}

/// Carriers keyed by their internal class name. Every selected artifact must parse.
fn carrier_abis(classes: &[(String, Vec<u8>)]) -> BTreeMap<String, CarrierAbi> {
    let mut carriers = BTreeMap::new();
    for (name, bytes) in classes {
        let info = krusty::jvm::classreader::parse_class(bytes)
            .unwrap_or_else(|error| panic!("parse {name}: {error:?}"));
        let Some(superclass) = info.super_class else {
            continue;
        };
        if !superclass.matches(CARRIER_SUPER) {
            continue;
        }
        let internal = info.this_class.render();
        let abi = CarrierAbi {
            access: info.access,
            superclass: superclass.render(),
            interfaces: info.interfaces(),
            fields: info
                .fields
                .iter()
                .map(|field| member_abi(&field.name, &field.descriptor, field.access))
                .collect(),
            methods: info
                .methods
                .iter()
                .map(|method| member_abi(&method.name, &method.descriptor, method.access))
                .collect(),
        };
        if carriers.insert(internal.clone(), abi).is_some() {
            panic!("duplicate fun-interface constructor carrier {internal}");
        }
    }
    carriers
}

fn compile_krusty(sources: &[(&str, &str)], label: &str) -> Vec<(String, Vec<u8>)> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let artifacts = common::compile_in_process_files(sources, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| {
            let files = sources
                .iter()
                .map(|(_, source)| *source)
                .collect::<Vec<_>>();
            let diagnostics = common::front_end_diagnostics_files_with_stdlib(&files);
            panic!("{label}: krusty rejected the sources: {diagnostics:?}");
        });
    selected_class_artifacts(artifacts)
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

fn one_carrier(class_name: &str, method: &str) -> BTreeMap<String, CarrierAbi> {
    BTreeMap::from([(
        class_name.to_string(),
        CarrierAbi {
            access: 0x1030,
            superclass: CARRIER_SUPER.to_string(),
            interfaces: vec!["kotlin/jvm/functions/Function1".to_string()],
            fields: vec![member_abi("INSTANCE", &format!("L{class_name};"), 0x0019)],
            methods: vec![member_abi(method, "()V", 0)],
        },
    )])
}

#[test]
fn renamed_extra_or_misattached_member_changes_the_carrier() {
    let base = one_carrier("FunIfaceCtorKt$kr$1", "<init>");
    let renamed = one_carrier("FunIfaceCtorKt$kr$1", "init");
    assert_ne!(base, renamed, "a renamed method is a different carrier");
    let mut extra = base.clone();
    extra
        .get_mut("FunIfaceCtorKt$kr$1")
        .expect("carrier")
        .methods
        .push(member_abi("extra", "()V", 0x0001));
    assert_ne!(base, extra, "an extra method is a different carrier");
    let mut other_class = BTreeMap::new();
    other_class.insert(
        "FunIfaceCtorKt$fir$function$0".to_string(),
        base["FunIfaceCtorKt$kr$1"].clone(),
    );
    assert_ne!(
        base, other_class,
        "the same member list on another class is a different carrier set"
    );
}

#[test]
fn kotlin_module_resource_is_excluded_before_parse() {
    let selected = selected_class_artifacts(vec![
        (
            "META-INF/main.kotlin_module".to_string(),
            b"not a class".to_vec(),
        ),
        ("FunIfaceCtorKt".to_string(), vec![0xCA, 0xFE, 0xBA, 0xBE]),
    ]);
    assert_eq!(
        selected
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["FunIfaceCtorKt"]
    );
}

#[test]
#[should_panic(expected = "parse Bad")]
fn malformed_class_header_fails() {
    let _ = carrier_abis(&[("Bad".to_string(), b"not-a-class".to_vec())]);
}

#[test]
fn unreadable_class_entry_fails() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!(
        "krusty-fun-iface-unreadable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("Broken.class");
    std::fs::write(&file, b"class").expect("temp class");
    let mut permissions = std::fs::metadata(&file).expect("metadata").permissions();
    permissions.set_mode(0);
    std::fs::set_permissions(&file, permissions).expect("permissions");
    let caught = std::panic::catch_unwind(|| class_bytes(&dir));
    let mut permissions = std::fs::metadata(&file).expect("metadata").permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&file, permissions).expect("restore");
    std::fs::remove_dir_all(&dir).expect("cleanup");
    assert!(
        caught.is_err(),
        "an unreadable class entry must fail the oracle"
    );
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
