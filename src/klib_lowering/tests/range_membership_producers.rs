//! Official serialized range-literal memberships this fold does not model.

use super::*;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_range_membership/Library.kt");

/// `value in 0L..10L` over an `Int` value calls the stdlib's `LongRange.contains(Int)`
/// extension: a valid declaration outside the same-element fold, so the body declines as
/// unsupported rather than reporting inconsistent serialized evidence.
#[test]
fn official_mixed_element_membership_declines_as_unsupported() {
    let Some(root) = distribution_root() else {
        return;
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let work = loop {
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "krusty-klib-range-membership-{}-{ordinal}",
            std::process::id()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => break directory,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create producer directory: {error}"),
        }
    };
    let source = work.join("Membership.kt");
    std::fs::write(&source, LIBRARY).unwrap();
    let library = work.join("membership");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library),
    ])
    .expect("read official serialized membership declarations");
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), "mixed");
    let [function] = symbols.callables.functions() else {
        panic!("one producer declaration")
    };
    let selected = libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap();
    let callable = selected.klib_body_callable().unwrap();
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function_with_intrinsics(
            callable,
            libraries.declaration_bodies(),
            &KlibCalleeFacts::new([callable]),
            libraries.intrinsic_facts(),
        )
        .expect_err("a mixed-element membership is outside the fold");
    assert_eq!(
        decline.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation(
            "a membership through a `contains` that is not a member of the range"
        )
    );
    assert!(!decline.is_internal_inconsistency());
    super::demo_package::assert_unit_is_empty(&unit);
    std::fs::remove_dir_all(work).unwrap();
}
