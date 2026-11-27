//! The `super` calls a library function's emitted body reaches: the function's own body, a local
//! or anonymous class it constructs, and the initializer of a top-level property it reads. The
//! unit emits the interface default each one names, so each must be in the frozen inventory.

use super::*;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};

/// The portable program's library module, which the consumer module's `box` calls.
const PROGRAM: &str = include_str!("../../../tests/box_regressions/librarySuperCallRoots.kt");

fn super_callables_of(libraries: &KlibLibraries, function: &str) -> Vec<Vec<String>> {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), function);
    let [function] = symbols.callables.functions() else {
        panic!("one library function `{function}`")
    };
    let identity = function
        .callable
        .external_identity
        .expect("a KLIB function carries its provider identity");
    let callable = libraries
        .external_callable(identity)
        .expect("the provider realizes the function");
    let selected = callable.klib_body_callable().expect("signed function");
    let body =
        crate::klib_lowering::emitted_body(libraries.declaration_bodies(), selected.identity());
    body.super_callables
        .iter()
        .map(|target| match &target.declaration {
            crate::libraries::KlibDeclarationSignature::Public(signature) => {
                signature.declaration().segments().to_vec()
            }
            other => panic!("a public `super` target, found {other:?}"),
        })
        .collect()
}

#[test]
fn a_super_call_in_a_library_anonymous_class_or_initializer_is_an_emitted_default() {
    let Some(root) = distribution_root() else {
        return;
    };
    let library_module = PROGRAM
        .split_once("// MODULE: main(lib)\n")
        .expect("the shared program declares a consumer module")
        .0
        .trim_start_matches("// MODULE: lib\n");
    let work = std::env::temp_dir().join(format!("krusty-super-call-roots-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let source = work.join("SuperCallRoots.kt");
    std::fs::write(&source, format!("package fixture\n{library_module}")).unwrap();
    let library = work.join("super-call-roots");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "producer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library),
    ])
    .unwrap();
    // The file's initializers are roots of every function of the file, so each function's
    // inventory also holds the default its initializer's `super` call names.
    let greeter = vec!["Greeter".to_owned(), "greet".to_owned()];
    let namer = vec!["Namer".to_owned(), "name".to_owned()];
    let anonymous = super_callables_of(&libraries, "anonymousGreeting");
    assert!(anonymous.contains(&greeter), "{anonymous:?}");
    let initializer = super_callables_of(&libraries, "fileInitializerName");
    assert!(initializer.contains(&namer), "{initializer:?}");
    std::fs::remove_dir_all(work).unwrap();
}
