use super::*;

fn version(text: &str) -> DumpVersion {
    parse_dump_version(text).unwrap_or_else(|| panic!("dump version {text}"))
}

fn files(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    map.insert("pkg/A".to_string(), bytes.to_vec());
    map
}

#[test]
fn only_releases_and_rc_tags_are_cached() {
    for text in [
        "2.4.20",
        "2.4.20-release-482",
        "2.4.20-RC",
        "2.4.20-RC2",
        "2.4.0-RC-137",
        "2.4.20-RC2-release-15",
        "2.4.20-RC2-15",
    ] {
        assert_eq!(
            cacheable_build_identity(text).as_deref(),
            Some(text),
            "{text}"
        );
    }
    for text in [
        "",
        "2.4",
        "2.4.20-SNAPSHOT",
        "2.3.255-SNAPSHOT",
        "2.4.20-dev-2181",
        "2.4.20-Beta1",
        "2.4.20-Beta1-release-3",
        "2.4.20-RC2-beta",
        "v2.4.20",
        "2.4.20-release",
    ] {
        assert_eq!(cacheable_build_identity(text), None, "{text}");
    }
    assert_eq!(
        parse_dump_version("2.4.20-release-482"),
        Some(DumpVersion {
            version: KotlinVersion::new(2, 4, 20),
            channel: Channel::Release,
        })
    );
    assert_eq!(
        parse_dump_version("2.4.20-RC2"),
        Some(DumpVersion {
            version: KotlinVersion::new(2, 4, 20),
            channel: Channel::Rc,
        })
    );
}

#[test]
fn an_open_range_covers_a_newer_release_without_rewriting() {
    let root = temp_root("open");
    let fingerprint = fingerprint_parts(&[b"source"]);
    let release = version("2.4.20");
    store_files(
        &root,
        "mod",
        "case|Stem|default|plain",
        release,
        fingerprint,
        &files(b"one"),
    );
    let index = disk_index(&root);
    assert!(index.contains("[[mod]]"), "{index}");
    assert!(index.contains("2.4.20.. "), "{index}");
    assert!(archive_path(&root).is_file());
    assert!(!index.contains("2.4.10"), "{index}");
    let newer = DumpVersion {
        version: KotlinVersion::new(2, 4, 30),
        channel: Channel::Release,
    };
    let hit = load_files(&root, "mod", "case|Stem|default|plain", newer, fingerprint);
    assert_eq!(hit.unwrap().get("pkg/A").unwrap(), b"one");
    store_files(
        &root,
        "mod",
        "case|Stem|default|plain",
        newer,
        fingerprint,
        &files(b"one"),
    );
    let again = disk_index(&root);
    assert_eq!(
        again, index,
        "matching bytes leave the open range untouched"
    );

    store_files(
        &root,
        "mod",
        "case|Stem|default|plain",
        newer,
        fingerprint,
        &files(b"two"),
    );
    let split = disk_index(&root);
    assert!(split.contains("2.4.20 "), "{split}");
    assert!(split.contains("2.4.30.. "), "{split}");
    assert_eq!(
        load_files(
            &root,
            "mod",
            "case|Stem|default|plain",
            release,
            fingerprint
        )
        .unwrap()
        .get("pkg/A")
        .unwrap(),
        b"one"
    );
    assert_eq!(
        load_files(&root, "mod", "case|Stem|default|plain", newer, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"two"
    );
    let rc = version("2.4.20-RC2");
    assert!(load_files(&root, "mod", "case|Stem|default|plain", rc, fingerprint).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_release_dump_is_reused_and_a_snapshot_never_is() {
    let root = temp_root("reuse");
    let fingerprint = fingerprint_parts(&[b"source"]);
    let mut compiles = 0u32;
    let compile = |compiles: &mut u32| {
        *compiles += 1;
        Some(files(b"class-a"))
    };
    let query = |compiler, fingerprint, force| Recall {
        root: &root,
        module: "mod",
        key: "case|Stem|default|plain",
        compiler,
        fingerprint,
        force,
        compile_missing: false,
        write: true,
    };
    let first = recall(
        query(Some(version("2.4.20-RC2")), fingerprint, true),
        |_| true,
        || compile(&mut compiles),
    );
    assert_eq!(first.unwrap().get("pkg/A").unwrap(), b"class-a");
    assert_eq!(compiles, 1);
    let second = recall(
        query(Some(version("2.4.20-RC")), fingerprint, false),
        |_| true,
        || compile(&mut compiles),
    );
    assert_eq!(second.unwrap().get("pkg/A").unwrap(), b"class-a");
    assert_eq!(compiles, 1, "a matching dump skips kotlinc");

    let changed = fingerprint_parts(&[b"source-v2"]);
    let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        recall(
            query(Some(version("2.4.20-RC2")), changed, false),
            |_| true,
            || compile(&mut compiles),
        )
    }));
    let message = panic_message(missing.expect_err("a new fingerprint must fail"));
    assert_eq!(
        message,
        expected_dump_miss(&format!(
            "module mod key case|Stem|default|plain fingerprint {changed:032x}"
        ))
    );
    assert_eq!(compiles, 1, "a missing release dump does not compile");

    let snapshot = recall(
        query(None, changed, false),
        |_| true,
        || compile(&mut compiles),
    );
    assert!(snapshot.is_some());
    assert_eq!(compiles, 2);
    let again = recall(
        query(None, changed, false),
        |_| true,
        || compile(&mut compiles),
    );
    assert!(again.is_some());
    assert_eq!(compiles, 3, "a snapshot never reuses a dump");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_release_dump_fails_without_compiling() {
    let root = temp_root("ci");
    let fingerprint = fingerprint_parts(&[b"source"]);
    let mut compiled = false;
    let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        recall(
            Recall {
                root: &root,
                module: "mod",
                key: "case|Stem|default|plain",
                compiler: Some(version("2.4.20-release-1")),
                fingerprint,
                force: false,
                compile_missing: false,
                write: false,
            },
            |_| true,
            || {
                compiled = true;
                Some(files(b"bytes"))
            },
        )
    }));
    let message = panic_message(missing.expect_err("a missing dump must fail the test"));
    assert_eq!(
        message,
        expected_dump_miss(&format!(
            "module mod key case|Stem|default|plain fingerprint {fingerprint:032x}"
        ))
    );
    assert!(!compiled, "a missing release dump does not compile");
    assert!(
        !archive_path(&root).exists(),
        "a miss that is not recorded leaves no dump"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_read_only_partial_cache_compiles_a_new_fingerprint_without_writing() {
    let root = temp_root("partial-read-only");
    let release = version("2.4.20");
    let key = "case|Stem|default|plain";
    let cached_fingerprint = fingerprint_parts(&[b"cached source"]);
    let new_fingerprint = fingerprint_parts(&[b"new source"]);
    store_files(
        &root,
        "mod",
        key,
        release,
        cached_fingerprint,
        &files(b"cached bytes"),
    );
    flush_archive(&root);
    let archive_before = std::fs::read(archive_path(&root)).expect("partial archive");
    let mut compiles = 0u32;

    let produced = recall(
        Recall {
            root: &root,
            module: "mod",
            key,
            compiler: Some(release),
            fingerprint: new_fingerprint,
            force: false,
            compile_missing: true,
            write: false,
        },
        |_| true,
        || {
            compiles += 1;
            Some(files(b"live bytes"))
        },
    )
    .expect("a read-only cache miss compiles live");

    assert_eq!(compiles, 1);
    assert_eq!(produced.get("pkg/A").unwrap(), b"live bytes");
    assert_eq!(
        std::fs::read(archive_path(&root)).expect("unchanged partial archive"),
        archive_before,
        "a read-only live compile must not change the restored archive"
    );
    assert!(
        load_files(&root, "mod", key, release, new_fingerprint).is_none(),
        "a read-only live compile must not add the new fingerprint"
    );
    assert_eq!(
        load_files(&root, "mod", key, release, cached_fingerprint)
            .expect("cached entry remains reusable")
            .get("pkg/A")
            .unwrap(),
        b"cached bytes"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_published_miss_stores_the_live_invocation_and_a_private_miss_does_not() {
    let root = temp_root("publish-miss");
    let src = root.join("Naming.kt");
    std::fs::write(&src, "fun box() = \"OK\"\n").unwrap();
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("NamingKt.class"), b"class-bytes").unwrap();
    let args = vec![
        "-d".to_string(),
        out.display().to_string(),
        src.display().to_string(),
    ];
    let release = version("2.4.20");
    super::remember_live_compile(&args, 0, "", &root, Some(release), true);
    let replayed = super::replay_class_dump_with_policy(&args, &root, Some(release), false, false)
        .expect("a stored miss replays");
    assert!(replayed.status);
    assert_eq!(replayed.code, 0);
    assert_eq!(
        replayed.files.get("NamingKt.class").unwrap(),
        b"class-bytes"
    );

    let other = root.join("Other.kt");
    std::fs::write(&other, "fun other() = 1\n").unwrap();
    let unpublished = vec![
        "-d".to_string(),
        out.display().to_string(),
        other.display().to_string(),
    ];
    super::remember_live_compile(&unpublished, 0, "", &root, Some(release), false);
    let missing =
        super::replay_class_dump_with_policy(&unpublished, &root, Some(release), false, true);
    assert!(
        missing.is_none(),
        "a miss that is not published compiles live instead of failing"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn distinct_class_sets_do_not_share_a_dump_key() {
    assert_ne!(classes_suffix(&["pkg/A"]), classes_suffix(&["pkg/B"]));
    assert_eq!(classes_suffix(&["pkg/B", "pkg/A"]), "#pkg/A,pkg/B");
    assert_ne!(classes_suffix(&["pkg/A"]), "#tree");
}

fn expected_dump_miss(detail: &str) -> String {
    let compiler = published_compiler_id().unwrap_or_else(|| "this release".to_string());
    format!(
        "the recorded-byte cache has no class dump for {detail} under kotlinc {compiler}. \
         This run does not compile with kotlinc for a release or RC that already uses the archive. \
         Refresh the master GitHub cache with KRUSTY_RECORD_CLASS_DUMPS=1."
    )
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
        })
        .unwrap_or_default()
}

#[test]
fn a_hit_missing_a_requested_class_fails_until_recorded() {
    let root = temp_root("missing");
    let fingerprint = fingerprint_parts(&[b"source"]);
    let release = version("2.4.20");
    let key = "case|Stem|default|plain";
    store_files(&root, "mod", key, release, fingerprint, &files(b"one"));
    let mut compiles = 0u32;
    let mut query = Recall {
        root: &root,
        module: "mod",
        key,
        compiler: Some(release),
        fingerprint,
        force: false,
        compile_missing: false,
        write: true,
    };
    let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        recall(
            query,
            |hit| hit.contains_key("pkg/Missing"),
            || {
                compiles += 1;
                Some(files(b"unused"))
            },
        )
    }));
    assert!(missing.is_err(), "an incomplete dump is not a hit");
    assert_eq!(compiles, 0, "an incomplete dump does not compile");
    query.force = true;
    let replaced = recall(
        query,
        |hit| hit.contains_key("pkg/Missing"),
        || {
            compiles += 1;
            let mut map = files(b"two");
            map.insert("pkg/Missing".to_string(), b"present".to_vec());
            Some(map)
        },
    );
    assert_eq!(compiles, 1);
    assert_eq!(replaced.unwrap().get("pkg/Missing").unwrap(), b"present");
    query.force = false;
    let reused = recall(
        query,
        |hit| hit.contains_key("pkg/Missing"),
        || {
            compiles += 1;
            Some(files(b"three"))
        },
    );
    assert_eq!(compiles, 1, "the completed dump is reused");
    assert_eq!(reused.unwrap().get("pkg/A").unwrap(), b"two");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_partial_invocation_cache_yields_a_new_source_to_a_read_only_live_compile() {
    let root = temp_root("partial-invocation");
    let source = root.join("Lib.kt");
    let out = root.join("out");
    let args = vec![
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    std::fs::write(&source, "fun cached() = 1\n").unwrap();
    let cached = parse_invocation(&args)
        .expect("cached invocation inputs")
        .expect("cached invocation");
    let mut cached_files = files(b"cached class bytes");
    attach_status(&mut cached_files, 0, "");
    let release = version("2.4.20");
    store_files(
        &root,
        INVOCATION_MODULE,
        &hex128(cached.fingerprint),
        release,
        cached.fingerprint,
        &cached_files,
    );
    let replayed = replay_class_dump_with_policy(&args, &root, Some(release), false, true)
        .expect("the existing source replays from the partial cache");
    assert_eq!(replayed.files.get("pkg/A").unwrap(), b"cached class bytes");
    flush_archive(&root);
    let archive_before = std::fs::read(archive_path(&root)).expect("partial archive");

    std::fs::write(&source, "fun added() = 2\n").unwrap();
    assert!(
        replay_class_dump_with_policy(&args, &root, Some(release), false, true).is_none(),
        "a new source fingerprint is yielded to the live compiler"
    );
    assert_eq!(
        std::fs::read(archive_path(&root)).expect("unchanged partial archive"),
        archive_before,
        "a read-only miss must not change the restored archive"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_jvm_property_is_part_of_the_invocation_fingerprint() {
    let root = temp_root("jvm-prop");
    let source = root.join("Lib.kt");
    std::fs::write(&source, "fun box() = \"OK\"\n").unwrap();
    let out = root.join("out").to_string_lossy().into_owned();
    let source = source.to_string_lossy().into_owned();
    let base = vec!["-d".to_string(), out, source];
    let with_property =
        std::iter::once("-Dkotlinc.test.allow.testonly.language.features=true".to_string())
            .chain(base.iter().cloned())
            .collect::<Vec<_>>();
    let other_property = std::iter::once("-Dother=1".to_string())
        .chain(base.iter().cloned())
        .collect::<Vec<_>>();
    let plain = parse_invocation(&base)
        .expect("plain inputs")
        .expect("plain invocation");
    let enabled = parse_invocation(&with_property)
        .expect("property inputs")
        .expect("property invocation");
    let other = parse_invocation(&other_property)
        .expect("other inputs")
        .expect("other invocation");
    assert_ne!(plain.fingerprint, enabled.fingerprint);
    assert_ne!(enabled.fingerprint, other.fingerprint);

    let mut stored = BTreeMap::new();
    stored.insert("MainKt.class".to_string(), b"class".to_vec());
    attach_status(&mut stored, 0, "");
    let release = version("2.4.20");
    store_files(
        &root,
        INVOCATION_MODULE,
        &hex128(enabled.fingerprint),
        release,
        enabled.fingerprint,
        &stored,
    );
    let hit = replay_class_dump_with_policy(&with_property, &root, Some(release), false, false)
        .expect("the property-bearing invocation replays");
    assert_eq!(hit.code, 0);
    assert_eq!(hit.files.get("MainKt.class").unwrap(), b"class");
    assert!(
        replay_class_dump_with_policy(&base, &root, Some(release), false, true).is_none(),
        "without the property the archive entry is a miss"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_invocation_fingerprint_ignores_the_output_directory() {
    let root = temp_root("inv");
    let source = root.join("Lib.kt");
    std::fs::write(&source, "fun box() = \"OK\"\n").unwrap();
    let args = |out: &str| {
        vec![
            "-d".to_string(),
            root.join(out).to_string_lossy().into_owned(),
            source.to_string_lossy().into_owned(),
        ]
    };
    let left = parse_invocation(&args("out-a"))
        .expect("left inputs")
        .expect("left invocation");
    let right = parse_invocation(&args("out-b"))
        .expect("right inputs")
        .expect("right invocation");
    assert_eq!(left.fingerprint, right.fingerprint);
    assert_ne!(left.out, right.out);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_common_sources_flag_ignores_its_scratch_path() {
    let root = temp_root("common-src");
    let left_dir = root.join("left");
    let right_dir = root.join("right");
    std::fs::create_dir_all(&left_dir).unwrap();
    std::fs::create_dir_all(&right_dir).unwrap();
    let text = "expect class A\n";
    std::fs::write(left_dir.join("Common.kt"), text).unwrap();
    std::fs::write(right_dir.join("Common.kt"), text).unwrap();
    let flag = |dir: &Path| {
        format!(
            "-Xcommon-sources={},{}",
            dir.join("Common.kt").display(),
            dir.join("Common.kt").display()
        )
    };
    assert_eq!(
        normalize_invocation_flag(&flag(&left_dir)).unwrap(),
        normalize_invocation_flag(&flag(&right_dir)).unwrap()
    );
    assert_ne!(flag(&left_dir), flag(&right_dir));
    std::fs::write(right_dir.join("Common.kt"), "expect class B\n").unwrap();
    assert_ne!(
        normalize_invocation_flag(&flag(&left_dir)).unwrap(),
        normalize_invocation_flag(&flag(&right_dir)).unwrap()
    );
    assert_eq!(
        normalize_invocation_flag("-jvm-target=17").unwrap(),
        "-jvm-target=17"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_panic_inside_recorded_diagnostics_restores_the_previous_flag() {
    REQUIRE_DIAGNOSTICS.with(|flag| flag.set(true));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_recorded_diagnostics(|| panic!("diagnostic assert failed"));
    }));
    assert_eq!(
        panic_message(panicked.expect_err("the scoped body panics")),
        "diagnostic assert failed"
    );
    assert!(diagnostics_required());
    REQUIRE_DIAGNOSTICS.with(|flag| flag.set(false));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_recorded_diagnostics(|| panic!("diagnostic assert failed"));
    }));
    assert!(panicked.is_err());
    assert!(!diagnostics_required());
}

#[test]
fn a_missing_plugin_or_classpath_entry_is_not_an_empty_fingerprint() {
    let root = temp_root("unreadable");
    let missing_plugin = root.join("missing-plugin.jar");
    let empty_plugin = root.join("empty-plugin.jar");
    std::fs::write(&empty_plugin, b"").unwrap();
    let missing_flag = format!("-Xplugin={}", missing_plugin.display());
    let empty_flag = format!("-Xplugin={}", empty_plugin.display());
    assert_eq!(
        normalize_invocation_flag(&missing_flag).unwrap_err(),
        format!(
            "unreadable plugin {}: No such file or directory (os error 2)",
            missing_plugin.display()
        )
    );
    let empty_plugin_key = normalize_invocation_flag(&empty_flag).unwrap();
    assert_eq!(
        empty_plugin_key,
        format!("-Xplugin={}", hex128(fingerprint_parts(&[b""])))
    );
    assert_ne!(missing_flag, empty_plugin_key);

    let plugin_dir = root.join("plugin-dir");
    std::fs::create_dir(&plugin_dir).unwrap();
    assert_eq!(
        normalize_invocation_flag(&format!("-Xplugin={}", plugin_dir.display())).unwrap_err(),
        format!(
            "unreadable plugin {}: Is a directory (os error 21)",
            plugin_dir.display()
        )
    );

    let missing_cp = root.join("missing-cp.jar");
    let empty_cp = root.join("empty-cp.jar");
    std::fs::write(&empty_cp, b"").unwrap();
    assert_eq!(
        classpath_content_fingerprint_with_platform(std::slice::from_ref(&missing_cp), None, None,)
            .unwrap_err(),
        format!(
            "unreadable classpath entry {}: No such file or directory (os error 2)",
            missing_cp.display()
        )
    );
    assert_eq!(
        classpath_content_fingerprint_with_platform(std::slice::from_ref(&empty_cp), None, None,)
            .unwrap(),
        format!("file:{:032x}", fingerprint_parts(&[b""]))
    );

    let empty_dir = root.join("empty-dir");
    std::fs::create_dir(&empty_dir).unwrap();
    let missing_dir = root.join("missing-dir");
    let empty_hash = hash_tree(&empty_dir).unwrap();
    assert_eq!(
        hash_tree(&missing_dir).unwrap_err(),
        format!(
            "unreadable directory {}: No such file or directory (os error 2)",
            missing_dir.display()
        )
    );
    assert_ne!(
        format!("dir:{empty_hash}"),
        hash_tree(&missing_dir).unwrap_err()
    );

    let source = root.join("Lib.kt");
    std::fs::write(&source, "fun box() = \"OK\"\n").unwrap();
    let args = vec![
        "-d".to_string(),
        root.join("out").to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
        format!("-cp={}", missing_cp.display()),
    ];
    let err = match parse_invocation(&args) {
        Err(err) => err,
        Ok(_) => panic!("missing classpath must not build a key"),
    };
    assert_eq!(
        err,
        format!(
            "unreadable classpath entry {}: No such file or directory (os error 2)",
            missing_cp.display()
        )
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_fingerprints_under_one_key_both_replay() {
    let root = temp_root("two-fp");
    let release = version("2.4.20");
    let first = fingerprint_parts(&[b"first"]);
    let second = fingerprint_parts(&[b"second"]);
    store_files(
        &root,
        "mod",
        "case|Stem|default|plain#tree",
        release,
        first,
        &files(b"one"),
    );
    store_files(
        &root,
        "mod",
        "case|Stem|default|plain#tree",
        release,
        second,
        &files(b"two"),
    );
    assert_eq!(
        load_files(&root, "mod", "case|Stem|default|plain#tree", release, first)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"one"
    );
    assert_eq!(
        load_files(
            &root,
            "mod",
            "case|Stem|default|plain#tree",
            release,
            second
        )
        .unwrap()
        .get("pkg/A")
        .unwrap(),
        b"two"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_rejected_invocation_replays_its_stderr_and_a_plain_dump_is_success() {
    let mut rejected = BTreeMap::new();
    rejected.insert("pkg/A.class".to_string(), b"class".to_vec());
    attach_status(&mut rejected, 1, "only named arguments");
    let replayed = split_replay(rejected);
    assert!(replayed.status);
    assert_eq!(replayed.code, 1);
    assert_eq!(replayed.stderr, "only named arguments");
    assert_eq!(replayed.files.get("pkg/A.class").unwrap(), b"class");
    assert!(!replayed.files.contains_key(EXIT_ENTRY));
    assert!(!replayed.files.contains_key(STDERR_ENTRY));

    let mut plain = BTreeMap::new();
    plain.insert("pkg/A.class".to_string(), b"class".to_vec());
    attach_status(&mut plain, 0, "");
    let replayed = split_replay(plain);
    assert!(replayed.status);
    assert_eq!(replayed.code, 0);
    assert_eq!(replayed.stderr, "");
    assert_eq!(replayed.files.get("pkg/A.class").unwrap(), b"class");

    let mut classes_only = BTreeMap::new();
    classes_only.insert("pkg/A.class".to_string(), b"class".to_vec());
    let replayed = split_replay(classes_only);
    assert!(!replayed.status);
    assert_eq!(replayed.code, 0);
    assert_eq!(replayed.stderr, "");
    let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_recorded_diagnostics(|| {
            if !replayed.status {
                refuse_missing_dump(
                    "sources Main.kt fingerprint abc (class files are recorded, but not the exit code and diagnostics)",
                );
            }
        });
    }));
    let message = panic_message(missing.expect_err("a class dump without diagnostics must fail"));
    assert_eq!(
        message,
        expected_dump_miss(
            "sources Main.kt fingerprint abc (class files are recorded, but not the exit code and diagnostics)",
        )
    );

    let root = temp_root("status");
    let out = root.join("out");
    let mut stored = BTreeMap::new();
    stored.insert("pkg/A.class".to_string(), b"class".to_vec());
    attach_status(&mut stored, 0, "");
    write_output(&out, &stored);
    assert_eq!(std::fs::read(out.join("pkg/A.class")).unwrap(), b"class");
    assert!(!out.join(EXIT_ENTRY).exists());
    assert!(!out.join(STDERR_ENTRY).exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_directory_classpath_ignores_its_own_name() {
    let root = temp_root("cp");
    for name in ["pid-111", "pid-222"] {
        let class = root.join(name).join("pkg").join("A.class");
        std::fs::create_dir_all(class.parent().unwrap()).unwrap();
        std::fs::write(&class, b"same-bytes").unwrap();
    }
    let left = class_dump_inputs("src", "default", &[], &[root.join("pid-111")]);
    let right = class_dump_inputs("src", "default", &[], &[root.join("pid-222")]);
    assert_eq!(left.fingerprint, right.fingerprint);
    let module = root
        .join("pid-111")
        .join("META-INF")
        .join("main.kotlin_module");
    std::fs::create_dir_all(module.parent().unwrap()).unwrap();
    std::fs::write(&module, b"idx").unwrap();
    let shifted = class_dump_inputs("src", "default", &[], &[root.join("pid-111")]);
    assert_ne!(left.fingerprint, shifted.fingerprint);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn directory_classpath_identity_frames_names_and_bytes() {
    let root = temp_root("framed-directory-cp");
    let left = root.join("left");
    let right = root.join("right");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    std::fs::write(left.join("a"), b"bc").unwrap();
    std::fs::write(right.join("ab"), b"c").unwrap();

    let left = classpath_content_fingerprint_with_platform(&[left], None, None).unwrap();
    let right = classpath_content_fingerprint_with_platform(&[right], None, None).unwrap();
    assert_ne!(
        left, right,
        "relative names and file bytes must have independent hash boundaries"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn classpath_keys_preserve_order_and_file_bytes() {
    let root = temp_root("ordered-cp");
    let first = root.join("first.jar");
    let second = root.join("second.jar");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    let forward = class_dump_inputs("src", "default", &[], &[first.clone(), second.clone()]);
    let reverse = class_dump_inputs("src", "default", &[], &[second, first]);
    assert_ne!(forward.fingerprint, reverse.fingerprint);

    let left_dir = root.join("left");
    let right_dir = root.join("right");
    std::fs::create_dir_all(&left_dir).unwrap();
    std::fs::create_dir_all(&right_dir).unwrap();
    let left = left_dir.join("plugin.jar");
    let right = right_dir.join("plugin.jar");
    std::fs::write(&left, b"left").unwrap();
    std::fs::write(&right, b"right").unwrap();
    let left = normalize_args(&[format!("-Xplugin={}", left.display())]).unwrap();
    let right = normalize_args(&[format!("-Xplugin={}", right.display())]).unwrap();
    assert_ne!(left, right);
    let left_cp = class_dump_inputs("src", "default", &[], &[left_dir.join("plugin.jar")]);
    let right_cp = class_dump_inputs("src", "default", &[], &[right_dir.join("plugin.jar")]);
    assert_ne!(left_cp.fingerprint, right_cp.fingerprint);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn arbitrary_platform_lookalikes_are_content_hashed() {
    let root = temp_root("platform-lookalike");
    let stdlib = root.join("kotlin-stdlib.jar");
    let modules = root.join("other-jdk").join("lib").join("modules");
    std::fs::create_dir_all(modules.parent().unwrap()).unwrap();
    std::fs::write(&stdlib, b"stdlib-a").unwrap();
    std::fs::write(&modules, b"modules-a").unwrap();
    let first =
        classpath_content_fingerprint_with_platform(&[stdlib.clone(), modules.clone()], None, None)
            .unwrap();
    std::fs::write(&stdlib, b"stdlib-b").unwrap();
    let second =
        classpath_content_fingerprint_with_platform(&[stdlib.clone(), modules.clone()], None, None)
            .unwrap();
    assert_ne!(first, second);
    std::fs::write(&modules, b"modules-b").unwrap();
    let third =
        classpath_content_fingerprint_with_platform(&[stdlib, modules], None, None).unwrap();
    assert_ne!(second, third);

    let missing = root.join("no-such").join("kotlin-stdlib.jar");
    assert_eq!(
        classpath_content_fingerprint_with_platform(std::slice::from_ref(&missing), None, None,)
            .unwrap_err(),
        format!(
            "unreadable classpath entry {}: No such file or directory (os error 2)",
            missing.display()
        )
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn selected_toolchain_entries_have_content_derived_stable_identities() {
    struct Install {
        root: PathBuf,
        lib: PathBuf,
        stdlib: PathBuf,
        reflect: PathBuf,
        modules: PathBuf,
        symbols: PathBuf,
    }

    fn install(
        label: &str,
        stdlib_byte: u8,
        reflect_byte: u8,
        modules_byte: u8,
        symbols_byte: u8,
    ) -> Install {
        let root = temp_root(label);
        let lib = root.join("kotlinc").join("lib");
        let stdlib = lib.join("kotlin-stdlib.jar");
        let reflect = lib.join("kotlin-reflect.jar");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(&stdlib, vec![stdlib_byte; 1024]).unwrap();
        std::fs::write(&reflect, vec![reflect_byte; 1024]).unwrap();

        let jdk = root.join("jdk");
        let modules = jdk.join("lib").join("modules");
        let symbols = jdk.join("lib").join("ct.sym");
        std::fs::create_dir_all(modules.parent().unwrap()).unwrap();
        std::fs::write(
            jdk.join("release"),
            b"JAVA_VERSION=\"21.0.1\"\nIMPLEMENTOR=\"A\"\n",
        )
        .unwrap();
        std::fs::write(&modules, vec![modules_byte; 4096]).unwrap();
        std::fs::write(&symbols, vec![symbols_byte; 2048]).unwrap();
        Install {
            root,
            lib,
            stdlib,
            reflect,
            modules,
            symbols,
        }
    }

    fn selected(install: &Install, paths: &[PathBuf]) -> String {
        classpath_content_fingerprint_with_platform(
            paths,
            Some(&install.lib),
            Some(&install.modules),
        )
        .unwrap()
    }

    let first_install = install("selected-platform-a", 1, 2, 3, 4);
    let equal_install = install("selected-platform-b", 1, 2, 3, 4);
    let kotlin_patch = install("selected-platform-kotlin-patch", 9, 2, 3, 4);
    let compiler_patch = install("selected-platform-compiler-patch", 1, 9, 3, 4);
    let jdk_patch = install("selected-platform-jdk-patch", 1, 2, 8, 4);
    let symbols_patch = install("selected-platform-symbols-patch", 1, 2, 3, 8);
    let paths = |install: &Install| {
        vec![
            install.stdlib.clone(),
            install.modules.clone(),
            install.symbols.clone(),
        ]
    };

    let first = selected(&first_install, &paths(&first_install));
    assert_eq!(first, selected(&first_install, &paths(&first_install)));
    for selected_input in [
        first_install.lib.as_path(),
        first_install.modules.as_path(),
        first_install.symbols.as_path(),
    ] {
        assert_eq!(
            selected_content_hash_count(selected_input),
            1,
            "repeated fixtures hash each selected installation input once: {}",
            selected_input.display()
        );
    }
    let equal = selected(&equal_install, &paths(&equal_install));
    assert_eq!(
        first, equal,
        "equal selected toolchain bytes have a location-independent identity"
    );
    assert!(first.contains("kotlinc:kotlin-stdlib.jar:"), "{first}");
    assert!(first.contains("jdk:modules:"), "{first}");
    assert!(first.contains("jdk:ct.sym:"), "{first}");
    assert_ne!(
        first,
        selected(&kotlin_patch, &paths(&kotlin_patch)),
        "a selected runtime patch invalidates the dump"
    );
    assert_ne!(
        first,
        selected(&compiler_patch, &paths(&compiler_patch)),
        "any patched selected kotlinc lib invalidates the distribution identity"
    );
    assert_ne!(
        first,
        selected(&jdk_patch, &paths(&jdk_patch)),
        "a selected modules patch invalidates the dump even under the same release label"
    );
    assert_ne!(
        first,
        selected(&symbols_patch, &paths(&symbols_patch)),
        "a selected ct.sym patch invalidates the dump even under the same release label"
    );
    assert_ne!(
        selected(
            &first_install,
            &[first_install.stdlib.clone(), first_install.modules.clone()]
        ),
        selected(
            &first_install,
            &[first_install.reflect.clone(), first_install.modules.clone()]
        ),
        "logical distribution entries remain distinct"
    );
    assert_ne!(
        selected(
            &first_install,
            &[first_install.stdlib.clone(), first_install.modules.clone()]
        ),
        selected(
            &first_install,
            &[first_install.modules.clone(), first_install.stdlib.clone()]
        ),
        "classpath declaration order remains significant"
    );

    // Canonicalize up front: the fingerprint canonicalizes the modules path, so the error it
    // builds names the RESOLVED jdk/release — on macOS temp_dir's /var is a symlink to
    // /private/var, and comparing against the unresolved spelling fails the assertion.
    let missing_release = temp_root("selected-platform-missing-release")
        .canonicalize()
        .unwrap();
    let missing_modules = missing_release.join("jdk/lib/modules");
    std::fs::create_dir_all(missing_modules.parent().unwrap()).unwrap();
    std::fs::write(&missing_modules, b"modules").unwrap();
    let error = classpath_content_fingerprint_with_platform(
        std::slice::from_ref(&missing_modules),
        None,
        Some(&missing_modules),
    )
    .unwrap_err();
    assert_eq!(
        error,
        format!(
            "unreadable selected JDK identity {}: No such file or directory (os error 2)",
            missing_release.join("jdk/release").display()
        )
    );

    for install in [
        first_install,
        equal_install,
        kotlin_patch,
        compiler_patch,
        jdk_patch,
        symbols_patch,
    ] {
        let _ = std::fs::remove_dir_all(install.root);
    }
    let _ = std::fs::remove_dir_all(missing_release);
}

#[test]
fn empty_classpath_replay_is_scoped_to_exact_selected_toolchain_bytes() {
    struct Install {
        root: PathBuf,
        lib: PathBuf,
        modules: PathBuf,
    }

    fn install(label: &str, compiler_byte: u8, modules_byte: u8) -> Install {
        let root = temp_root(label);
        let lib = root.join("kotlinc/lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("kotlin-compiler.jar"), vec![compiler_byte; 1024]).unwrap();

        let jdk = root.join("jdk");
        let modules = jdk.join("lib/modules");
        std::fs::create_dir_all(modules.parent().unwrap()).unwrap();
        std::fs::write(
            jdk.join("release"),
            b"JAVA_VERSION=\"21.0.1\"\nIMPLEMENTOR=\"A\"\n",
        )
        .unwrap();
        std::fs::write(&modules, vec![modules_byte; 4096]).unwrap();
        Install { root, lib, modules }
    }

    fn inputs(install: &Install) -> u128 {
        class_dump_inputs_with_platform(
            "fun box() = \"OK\"",
            "default",
            &[],
            &[],
            Some(&install.lib),
            Some(&install.modules),
        )
        .fingerprint
    }

    let selected = install("empty-cp-selected", 1, 2);
    let equal = install("empty-cp-equal", 1, 2);
    let compiler_patch = install("empty-cp-compiler-patch", 9, 2);
    let jdk_patch = install("empty-cp-jdk-patch", 1, 9);
    let selected_inputs = inputs(&selected);
    let equal_inputs = inputs(&equal);
    let compiler_patch_inputs = inputs(&compiler_patch);
    let jdk_patch_inputs = inputs(&jdk_patch);

    assert_eq!(
        selected_inputs, equal_inputs,
        "equal ambient toolchain bytes have a location-independent empty-classpath key"
    );
    assert_ne!(
        selected_inputs, compiler_patch_inputs,
        "patched compiler bytes invalidate an empty-classpath lookup"
    );
    assert_ne!(
        selected_inputs, jdk_patch_inputs,
        "patched JDK bytes invalidate an empty-classpath lookup"
    );

    let unrelated_root = temp_root("ambient-toolchain-unrelated-cp");
    let unrelated = unrelated_root.join("dependency.jar");
    std::fs::write(&unrelated, b"same arbitrary dependency").unwrap();
    let unrelated_inputs = |install: &Install| {
        class_dump_inputs_with_platform(
            "fun box() = \"OK\"",
            "default",
            &[],
            std::slice::from_ref(&unrelated),
            Some(&install.lib),
            Some(&install.modules),
        )
    };
    assert_ne!(
        unrelated_inputs(&selected).fingerprint,
        unrelated_inputs(&compiler_patch).fingerprint,
        "an unrelated classpath does not erase the ambient compiler identity"
    );
    assert_ne!(
        unrelated_inputs(&selected).fingerprint,
        unrelated_inputs(&jdk_patch).fingerprint,
        "an unrelated classpath does not erase the ambient JDK identity"
    );
    assert_eq!(
        selected_content_hash_count(&selected.lib),
        1,
        "empty and unrelated classpaths share the memoized compiler identity"
    );
    assert_eq!(
        selected_content_hash_count(&selected.modules),
        1,
        "empty and unrelated classpaths share the memoized JDK identity"
    );

    let archive = temp_root("empty-cp-replay");
    let release = version("2.4.20");
    let key = "case|EmptyClasspath|default|plain";
    store_files(
        &archive,
        "mod",
        key,
        release,
        selected_inputs,
        &files(b"selected-toolchain"),
    );
    flush_archive(&archive);
    assert_eq!(
        load_files(&archive, "mod", key, release, equal_inputs)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"selected-toolchain"
    );
    assert!(
        load_files(&archive, "mod", key, release, compiler_patch_inputs).is_none(),
        "an empty-classpath replay must miss after a compiler patch"
    );
    assert!(
        load_files(&archive, "mod", key, release, jdk_patch_inputs).is_none(),
        "an empty-classpath replay must miss after a JDK patch"
    );

    for install in [selected, equal, compiler_patch, jdk_patch] {
        let _ = std::fs::remove_dir_all(install.root);
    }
    let _ = std::fs::remove_dir_all(unrelated_root);
    let _ = std::fs::remove_dir_all(archive);
}

#[test]
fn a_blob_round_trips_and_is_smaller_than_the_class_bytes() {
    let payload = b"kotlin/Metadata".repeat(200);
    let mut map = BTreeMap::new();
    map.insert("pkg/A.class".to_string(), payload.clone());
    map.insert(
        "META-INF/main.kotlin_module".to_string(),
        b"module".to_vec(),
    );
    let mut second = BTreeMap::new();
    second.insert("pkg/B.class".to_string(), payload);
    let archive = Archive::from_parts(
        BTreeMap::new(),
        [(1u128, encode_raw(&map)), (2, encode_raw(&second))]
            .into_iter()
            .collect(),
    );
    assert!(compress(&archive.body).len() < archive.body.len());
    assert_eq!(decode_raw(archive.blob(1).unwrap()), Some(map));
    assert_eq!(decode_raw(archive.blob(2).unwrap()), Some(second));
}

#[test]
fn every_module_shares_one_archive() {
    let root = temp_root("one");
    let fingerprint = fingerprint_parts(&[b"source"]);
    let release = version("2.4.20");
    store_files(
        &root,
        "mod",
        "case|A|default|plain",
        release,
        fingerprint,
        &files(b"a"),
    );
    store_files(
        &root,
        "other",
        "case|B|default|plain",
        release,
        fingerprint,
        &files(b"b"),
    );
    let index = disk_index(&root);
    assert!(index.contains("[[mod]]"), "{index}");
    assert!(index.contains("[[other]]"), "{index}");
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        1,
        "modules share one archive file"
    );
    assert_eq!(
        load_files(&root, "mod", "case|A|default|plain", release, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"a"
    );
    assert_eq!(
        load_files(&root, "other", "case|B|default|plain", release, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"b"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dumps_stay_in_memory_until_the_process_publishes_the_archive() {
    let root = temp_root("defer");
    let release = version("2.4.20");
    let first = fingerprint_parts(&[b"one"]);
    let second = fingerprint_parts(&[b"two"]);
    store_files(
        &root,
        "mod",
        "case|A|default|plain",
        release,
        first,
        &files(b"a"),
    );
    store_files(
        &root,
        "mod",
        "case|B|default|plain",
        release,
        second,
        &files(b"b"),
    );
    assert!(
        !archive_path(&root).exists(),
        "a dump is not written after each store"
    );
    assert_eq!(
        load_files(&root, "mod", "case|A|default|plain", release, first)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"a"
    );
    assert_eq!(
        load_files(&root, "mod", "case|B|default|plain", release, second)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"b"
    );
    flush_archive(&root);
    let published = std::fs::read(archive_path(&root)).expect("published archive");
    store_files(
        &root,
        "mod",
        "case|A|default|plain",
        release,
        first,
        &files(b"a"),
    );
    flush_archive(&root);
    assert_eq!(
        std::fs::read(archive_path(&root)).expect("unchanged archive"),
        published,
        "an unchanged dump does not rewrite the archive"
    );

    let foreign_root = temp_root("defer-foreign");
    let foreign = fingerprint_parts(&[b"foreign"]);
    store_files(
        &foreign_root,
        "mod",
        "case|C|default|plain",
        release,
        foreign,
        &files(b"c"),
    );
    flush_archive(&foreign_root);
    let foreign_bytes = std::fs::read(archive_path(&foreign_root)).expect("foreign archive");
    store_files(
        &root,
        "mod",
        "case|D|default|plain",
        release,
        fingerprint_parts(&[b"local"]),
        &files(b"d"),
    );
    std::fs::write(archive_path(&root), &foreign_bytes).expect("replace archive");
    flush_archive(&root);
    assert_eq!(
        load_files(&root, "mod", "case|C|default|plain", release, foreign)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"c",
        "a flush keeps entries another process published"
    );
    assert_eq!(
        load_files(
            &root,
            "mod",
            "case|D|default|plain",
            release,
            fingerprint_parts(&[b"local"])
        )
        .unwrap()
        .get("pkg/A")
        .unwrap(),
        b"d",
        "a flush keeps dumps this process has not published"
    );
    assert!(
        load_files(&root, "mod", "case|A|default|plain", release, first).is_none(),
        "a replaced archive does not keep entries this process already published"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&foreign_root);
}

#[test]
fn concurrent_versions_under_one_key_survive_publication_replay() {
    let root = temp_root("concurrent-version");
    let foreign_root = temp_root("concurrent-version-foreign");
    let key = "case|A|default|plain";
    let fingerprint = fingerprint_parts(&[b"same inputs"]);
    let local = version("2.4.10");
    let foreign = version("2.4.20");

    store_files(
        &root,
        "mod",
        key,
        version("2.4.0"),
        fingerprint,
        &files(b"base"),
    );
    flush_archive(&root);
    store_files(&root, "mod", key, local, fingerprint, &files(b"local"));

    store_files(
        &foreign_root,
        "mod",
        key,
        foreign,
        fingerprint,
        &files(b"foreign"),
    );
    flush_archive(&foreign_root);
    std::fs::write(
        archive_path(&root),
        std::fs::read(archive_path(&foreign_root)).unwrap(),
    )
    .unwrap();
    flush_archive(&root);

    assert_eq!(
        load_files(&root, "mod", key, local, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"local"
    );
    assert_eq!(
        load_files(&root, "mod", key, foreign, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"foreign"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&foreign_root);
}

#[test]
fn dirty_publication_reloads_a_replacement_with_an_equal_cached_stamp() {
    let root = temp_root("equal-stamp-publication");
    let foreign_root = temp_root("equal-stamp-publication-foreign");
    let key = "case|A|default|plain";
    let fingerprint = fingerprint_parts(&[b"same inputs"]);
    let local = version("2.4.10");
    let foreign = version("2.4.20");

    store_files(
        &root,
        "mod",
        key,
        version("2.4.0"),
        fingerprint,
        &files(b"base"),
    );
    flush_archive(&root);
    store_files(&root, "mod", key, local, fingerprint, &files(b"local"));

    store_files(
        &foreign_root,
        "mod",
        key,
        foreign,
        fingerprint,
        &files(b"foreign"),
    );
    flush_archive(&foreign_root);
    let path = archive_path(&root);
    std::fs::write(&path, std::fs::read(archive_path(&foreign_root)).unwrap()).unwrap();

    // Force even the complete cached generation to equal the replacement while retaining the
    // stale archive snapshot. Publication must still reload the locked file before it replays the
    // local pending record: the lock, not an unlocked observation, is its authority.
    let replacement_stamp = file_stamp(&path).unwrap();
    let mut cache = dump_cache().lock().unwrap();
    let slot = cache.get_mut(&path).unwrap();
    assert!(!slot.pending.is_empty());
    assert!(
        slot.archive
            .modules
            .get("mod")
            .and_then(|entries| entries.get(key))
            .is_some_and(|spans| spans.iter().all(|span| span.lo != foreign.version)),
        "the in-memory snapshot must be stale for this regression"
    );
    slot.stamp = Some(replacement_stamp);
    drop(cache);

    flush_archive(&root);

    assert_eq!(
        load_files(&root, "mod", key, local, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"local"
    );
    assert_eq!(
        load_files(&root, "mod", key, foreign, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"foreign",
        "an equal metadata stamp must not hide another process's publication"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&foreign_root);
}

#[test]
fn clean_observation_reloads_an_equal_metadata_atomic_replacement() {
    let root = temp_root("clean-equal-stamp-replacement");
    let replacement_root = temp_root("clean-equal-stamp-replacement-new");
    let release = version("2.4.20");
    let fingerprint = fingerprint_parts(&[b"same inputs"]);
    let key = "case|A|default|plain";

    store_files(&root, "mod", key, release, fingerprint, &files(b"old"));
    flush_archive(&root);
    assert_eq!(
        load_files(&root, "mod", key, release, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"old"
    );

    store_files(
        &replacement_root,
        "mod",
        key,
        release,
        fingerprint,
        &files(b"new"),
    );
    flush_archive(&replacement_root);
    let path = archive_path(&root);
    write_atomic(
        &path,
        &std::fs::read(archive_path(&replacement_root)).unwrap(),
    );
    make_cached_metadata_equal_but_keep_prior_identity(&path);

    assert_eq!(
        load_files(&root, "mod", key, release, fingerprint)
            .unwrap()
            .get("pkg/A")
            .unwrap(),
        b"new",
        "file identity must expose a replacement whose ordinary metadata matches the cache"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&replacement_root);
}

#[test]
fn clean_observation_rejects_an_equal_metadata_corrupt_atomic_replacement() {
    let root = temp_root("clean-equal-stamp-corrupt-replacement");
    let release = version("2.4.20");
    let fingerprint = fingerprint_parts(&[b"same inputs"]);
    let key = "case|A|default|plain";

    store_files(&root, "mod", key, release, fingerprint, &files(b"old"));
    flush_archive(&root);
    assert!(load_files(&root, "mod", key, release, fingerprint).is_some());
    let path = archive_path(&root);
    let len = std::fs::metadata(&path).unwrap().len() as usize;
    write_atomic(&path, &vec![0; len]);
    make_cached_metadata_equal_but_keep_prior_identity(&path);

    assert!(
        load_files(&root, "mod", key, release, fingerprint).is_none(),
        "a same-size corrupt replacement is a miss, never stale cached bytes"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deletion_and_corruption_never_resurrect_cached_archive_state() {
    let release = version("2.4.20");
    let fingerprint = fingerprint_parts(&[b"source"]);
    let key = "case|A|default|plain";

    let deleted = temp_root("deleted-archive");
    store_files(&deleted, "mod", key, release, fingerprint, &files(b"old"));
    flush_archive(&deleted);
    assert!(load_files(&deleted, "mod", key, release, fingerprint).is_some());
    std::fs::remove_file(archive_path(&deleted)).unwrap();
    assert!(
        load_files(&deleted, "mod", key, release, fingerprint).is_none(),
        "a clean in-memory snapshot must not outlive disk deletion"
    );

    let corrupt = temp_root("corrupt-archive");
    store_files(&corrupt, "mod", key, release, fingerprint, &files(b"old"));
    flush_archive(&corrupt);
    assert!(load_files(&corrupt, "mod", key, release, fingerprint).is_some());
    std::fs::write(archive_path(&corrupt), b"not a zlib archive").unwrap();
    assert!(
        load_files(&corrupt, "mod", key, release, fingerprint).is_none(),
        "corruption is a cache miss, not stale memory or a panic"
    );

    let pending = temp_root("deleted-pending-archive");
    store_files(&pending, "mod", key, release, fingerprint, &files(b"old"));
    flush_archive(&pending);
    store_files(
        &pending,
        "mod",
        "case|B|default|plain",
        release,
        fingerprint_parts(&[b"new"]),
        &files(b"new"),
    );
    std::fs::remove_file(archive_path(&pending)).unwrap();
    flush_archive(&pending);
    assert!(
        !archive_path(&pending).exists(),
        "pending memory must not recreate a deliberately deleted archive"
    );

    let pending_corrupt = temp_root("corrupt-pending-archive");
    store_files(
        &pending_corrupt,
        "mod",
        key,
        release,
        fingerprint,
        &files(b"old"),
    );
    flush_archive(&pending_corrupt);
    store_files(
        &pending_corrupt,
        "mod",
        "case|B|default|plain",
        release,
        fingerprint_parts(&[b"new"]),
        &files(b"new"),
    );
    let corrupt_bytes = b"replacement is corrupt";
    std::fs::write(archive_path(&pending_corrupt), corrupt_bytes).unwrap();
    flush_archive(&pending_corrupt);
    assert_eq!(
        std::fs::read(archive_path(&pending_corrupt)).unwrap(),
        corrupt_bytes,
        "pending memory must not overwrite a corrupt concurrent replacement"
    );

    let _ = std::fs::remove_dir_all(&deleted);
    let _ = std::fs::remove_dir_all(&corrupt);
    let _ = std::fs::remove_dir_all(&pending);
    let _ = std::fs::remove_dir_all(&pending_corrupt);
}

#[test]
fn the_dump_archive_is_outside_the_repository_inputs() {
    let root = dumps_root();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_ne!(root, manifest.join("tests"));
    assert!(!manifest.join("tests/recorded-bytes.zz").exists());
}

fn disk_index(root: &Path) -> String {
    flush_archive(root);
    let raw = decompress(&std::fs::read(archive_path(root)).unwrap());
    let end = raw.iter().position(|byte| *byte == 0).unwrap();
    String::from_utf8(raw[..end].to_vec()).unwrap()
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "krusty-byte-dump-{label}-{}-{}",
        std::process::id(),
        fingerprint_parts(&[label.as_bytes()])
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn make_cached_metadata_equal_but_keep_prior_identity(path: &Path) {
    let replacement = file_stamp(path).expect("replacement stamp");
    let mut cache = dump_cache().lock().unwrap();
    let slot = cache.get_mut(path).expect("clean cached archive");
    assert!(slot.pending.is_empty());
    let prior = slot.stamp.expect("prior archive stamp");
    assert_ne!(
        (prior.device, prior.inode),
        (replacement.device, replacement.inode),
        "an atomic replacement has a distinct file identity"
    );
    slot.stamp = Some(Stamp {
        device: prior.device,
        inode: prior.inode,
        ..replacement
    });
}
