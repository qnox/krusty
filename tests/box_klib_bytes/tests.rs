use super::*;

const SIMPLE: &str = include_str!("../box_regressions/klibByteEquality.kt");

#[test]
fn reference_cache_requires_a_complete_matching_inventory() {
    let root = Scratch::new().unwrap();
    let path = root.0.join("entries.zip");
    let entries = BTreeMap::from([
        ("lib/default/manifest".into(), vec![1, 2]),
        ("main/default/manifest".into(), vec![3, 4]),
    ]);
    reference_cache::store(&path, 17, &entries).unwrap();
    assert_eq!(reference_cache::load(&path, 17), Some(entries));
    assert_eq!(reference_cache::load(&path, 18), None);
    std::fs::write(&path, b"truncated ZIP").unwrap();
    assert_eq!(reference_cache::load(&path, 17), None);
}

#[test]
fn case_capture_keeps_module_identity_and_clears_after_failure() {
    let capture = emission_capture::Capture::new(true);
    emission_capture::record("lib", &[("default/manifest".into(), vec![1])]);
    emission_capture::record("main", &[("default/manifest".into(), vec![2])]);
    assert_eq!(
        capture.finish().unwrap(),
        QualifiedArtifacts::from([
            (("lib".into(), "default/manifest".into()), vec![1]),
            (("main".into(), "default/manifest".into()), vec![2]),
        ])
    );
    assert!(!emission_capture::active());
    let capture = emission_capture::Capture::new(true);
    emission_capture::error("metadata emission failed".into());
    assert_eq!(capture.finish(), Err("metadata emission failed".into()));
    assert!(!emission_capture::active());
}

#[test]
fn observing_klib_entries_preserves_executable_bytes() {
    use krusty::backend::Entry;
    use krusty::wasm::{WasmBackend, WasmTarget};
    let source = SIMPLE;
    let sources = vec![("Case".to_owned(), source.to_owned())];
    let backend = WasmBackend::new(WasmTarget::Js).with_entry(Entry::Box);
    let compile = || {
        box_lane::compile(
            &sources,
            source,
            box_lane::Libraries::StdlibKlib,
            TestTarget::WasmJs,
            &backend,
            "box",
            "krusty: the wasm backend does not support ",
        )
        .expect("compile the same program")
    };
    let plain = compile();
    let capture = emission_capture::Capture::new(true);
    let observed = compile();
    let entries = capture.finish().expect("capture KLIB from checked IR");
    assert_eq!(observed.artifacts, plain.artifacts);
    assert!(entries.contains_key(&("box".into(), "default/manifest".into())));
}

#[test]
fn module_entries_do_not_collide_and_missing_entries_get_no_credit() {
    let reference = QualifiedArtifacts::from([
        (("lib".into(), "default/manifest".into()), vec![1, 2]),
        (("main".into(), "default/manifest".into()), vec![3, 4]),
    ]);
    let actual = QualifiedArtifacts::from([
        (("lib".into(), "default/manifest".into()), vec![1, 2]),
        (("main".into(), "default/manifest".into()), vec![3, 9]),
        (("main".into(), "extra".into()), vec![5]),
    ]);
    assert_eq!(
        box_byte_score::score_passing(&actual, &reference),
        ByteScore {
            matched: 3,
            total: 5
        }
    );
    assert_eq!(
        box_byte_score::score_failed(&reference),
        ByteScore {
            matched: 0,
            total: 4
        }
    );
}

#[test]
fn planned_units_preserve_friend_and_common_source_boundaries() {
    let source = "// MODULE: common\nexpect fun answer(): String\n// MODULE: lib()()(common)\nactual fun answer() = \"OK\"\n// MODULE: main(lib)(lib)\nfun box() = answer()\n";
    let units = units(source, "Case", TestTarget::Native).unwrap();
    assert_eq!(units.len(), 2);
    assert_eq!(units[0].name, "lib");
    assert_eq!(units[0].common_file_count, 1);
    assert_eq!(units[1].deps, ["lib"]);
    assert_eq!(units[1].friends, ["lib"]);
}

#[test]
fn dependency_closures_keep_transitives_and_only_declared_friends() {
    let mut unit = units("fun box() = \"OK\"", "Case", TestTarget::Native)
        .unwrap()
        .remove(0);
    unit.deps = vec!["lib".into()];
    unit.friends = vec!["lib".into()];
    let built = BTreeMap::from([(
        "lib".into(),
        BuiltModule {
            path: "lib".into(),
            dependencies: vec!["base".into()],
        },
    )]);
    assert_eq!(
        dependencies(&unit, &built),
        Ok((vec!["lib".into(), "base".into()], vec!["lib".into()]))
    );
}

#[test]
fn cache_identity_separates_platform_version_content_source_and_topology() {
    let source = "fun box() = \"OK\"";
    let modules = units(source, "Case", TestTarget::WasmJs).unwrap();
    let key = reference::key(
        TestTarget::WasmJs,
        source,
        &modules,
        "compiler-2.4.21:contents-A",
    );
    assert_ne!(
        key,
        reference::key(
            TestTarget::WasmWasi,
            source,
            &modules,
            "compiler-2.4.21:contents-A"
        )
    );
    assert_ne!(
        key,
        reference::key(
            TestTarget::WasmJs,
            source,
            &modules,
            "compiler-2.4.20:contents-A"
        )
    );
    assert_ne!(
        key,
        reference::key(
            TestTarget::WasmJs,
            source,
            &modules,
            "compiler-2.4.21:contents-B"
        )
    );
    assert_ne!(
        key,
        reference::key(
            TestTarget::WasmJs,
            "fun box() = \"changed\"",
            &modules,
            "compiler-2.4.21:contents-A"
        )
    );
    let mut friends = modules.clone();
    friends[0].friends.push("lib".into());
    assert_ne!(
        key,
        reference::key(
            TestTarget::WasmJs,
            source,
            &friends,
            "compiler-2.4.21:contents-A"
        )
    );
    assert_eq!(
        key,
        reference::key(
            TestTarget::WasmJs,
            source,
            &modules,
            "compiler-2.4.21:contents-A"
        )
    );
}

#[test]
fn reference_library_entries_and_krusty_metadata_are_compared() {
    let target = match std::env::var("KRUSTY_KLIB_BYTE_TEST_TARGET").as_deref() {
        Ok("native") => TestTarget::Native,
        Ok("wasm-wasi") => TestTarget::WasmWasi,
        Ok("wasm-js") | Err(_) => TestTarget::WasmJs,
        other => panic!("unknown KLIB byte test target {other:?}"),
    };
    for source in [
        SIMPLE,
        include_str!("../box_regressions/multimodule/packagedFinalModuleBox.kt"),
        include_str!("../box_regressions/multimodule/multimoduleFriendDependency.kt"),
        include_str!("../box_regressions/multimodule/multimoduleDependsOnFriendDependency.kt"),
    ] {
        compare_entries(target, source);
    }
    let source = "fun box() = missing\n";
    let modules = units(source, "Case", target).unwrap();
    let error = reference::compile(target, source, &modules).unwrap_err();
    assert_eq!(
        super::super::common::reference_diagnostics::reference_error_blocks_from(&error),
        ["Case.kt:1:13: unresolved reference 'missing'."]
    );
    assert_eq!(
        reference::compile(target, source, &modules).unwrap_err(),
        error
    );
}

fn compare_entries(target: TestTarget, source: &str) {
    super::super::box_reference_classes::checked_box(source, "Case")
        .expect("the whole portable program must return OK through the JVM/kotlinc oracle");
    let modules = units(source, "Case", target).unwrap();
    let (reference, _) =
        reference::compile(target, source, &modules).expect("exact-version KLIB reference");
    let actual = emission::compile(target, source, &modules).expect("krusty KLIB output");
    for module in &modules {
        assert!(reference.contains_key(&(module.name.clone(), "default/manifest".into())));
        assert!(actual.contains_key(&(module.name.clone(), "default/manifest".into())));
    }
    let score = box_byte_score::score_passing(&actual, &reference);
    assert!(score.total > 0);
    assert!(score.matched > 0);
    let (replay, replayed) =
        reference::compile(target, source, &modules).expect("replay exact KLIB bytes");
    assert!(
        replayed,
        "a warm exact-input reference must not invoke kotlinc"
    );
    assert_eq!(replay, reference);
}

#[test]
fn reference_library_retains_upstream_hmpp_modules_with_javascript_resources() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let source = std::fs::read_to_string(corpus.join("multiplatform/k2/hmpp/kt83572.kt")).unwrap();
    let target = TestTarget::WasmJs;
    let modules = units(&source, "kt83572", target).expect("complete upstream source graph");
    assert_eq!(
        modules
            .iter()
            .map(|unit| unit.name.as_str())
            .collect::<Vec<_>>(),
        ["platform"]
    );
    assert_eq!(modules[0].common_file_count, 2);
    let (entries, _) = reference::compile(target, &source, &modules)
        .expect("original upstream program compiles as KLIB");
    assert!(entries.contains_key(&("platform".into(), "default/manifest".into())));
}
