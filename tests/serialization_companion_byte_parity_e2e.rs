//! Structural parity for the `Companion` the serialization plugin synthesizes on a `@Serializable`
//! class — the single most common generated class in a serialization-heavy program (one per
//! `@Serializable` declaration), and small enough to be byte-identical outright.
//!
//! Two divergences from kotlinc, both invisible to a round-trip test because the program serializes
//! correctly either way:
//!
//!   * `serializer()` returned the `$serializer` singleton with no `checkcast` to `KSerializer`. The
//!     accessor is declared to return `KSerializer<Foo>` while the singleton's own type is
//!     `Foo$serializer`, and kotlinc narrows at the return. The JVM verifies the method without it.
//!   * the `InnerClasses` entry for `$serializer` did not carry `ACC_SYNTHETIC`. The class's OWN
//!     access flags already had it; the entry is read independently (reflection consults the entry,
//!     not the class file it names), so the two must agree.
//!
//! The reference is kotlinc running its own serialization plugin from the SAME distribution — a
//! plugin from another Kotlin version would generate something else to diff against.
//!
//! The comparison includes `LineNumberTable` and `LocalVariableTable`: a plugin-generated member
//! carries both, mapped to the annotated owner's declaration line. Only constant-pool indices are
//! erased because their numbering is an emission-order artifact rather than class structure.
use std::path::PathBuf;

use super::common;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Point(val x: Int, val y: String)\n";

/// The newest `<artifact>-<version>.jar` Gradle cached for one exact module coordinate.
fn gradle_module_jar(group: &str, artifact: &str) -> Option<PathBuf> {
    let artifact_dir = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join(".gradle/caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for version in std::fs::read_dir(&artifact_dir).ok()?.flatten() {
        let name = version.file_name();
        let Some(name) = name.to_str() else { continue };
        let key = name
            .split(|character: char| !character.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>();
        let jar_name = format!("{artifact}-{name}.jar");
        for hash in std::fs::read_dir(version.path()).ok()?.flatten() {
            let jar = hash.path().join(&jar_name);
            if jar.is_file() && best.as_ref().is_none_or(|(current, _)| key > *current) {
                best = Some((key.clone(), jar));
            }
        }
    }
    best.map(|(_, jar)| jar)
}

/// Locate a compiler-plugin jar shipped beside the provisioned reference compiler.
fn kotlinc_plugin_jar(substring: &str) -> Option<PathBuf> {
    let lib = common::kotlin_compiler_jar()?.parent()?.to_path_buf();
    std::fs::read_dir(lib)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            (name.contains(substring) && name.ends_with(".jar")).then_some(path)
        })
}

struct ReferenceComparison {
    reference: String,
    krusty: String,
    reference_bytes: Vec<u8>,
    krusty_bytes: Vec<u8>,
}

/// Build one class with the reference serialization plugin and with krusty.
fn compare_with_kotlinc_plugin(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
    jvm_target: &str,
    kotlinc_extra: &[String],
) -> Option<ReferenceComparison> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&krusty_dir).ok()?;
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).ok()?;

    let mut arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        jvm_target.to_string(),
    ];
    if !cp_jars.is_empty() {
        arguments.push("-classpath".to_string());
        arguments.push(
            cp_jars
                .iter()
                .map(|jar| jar.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(":"),
        );
    }
    arguments.extend(kotlinc_extra.iter().cloned());
    arguments.push(source.to_string_lossy().into_owned());
    let (code, stderr) = common::kotlinc_compile(&arguments)?;
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");

    let class_major = jvm_target
        .parse::<u16>()
        .ok()
        .filter(|target| (9..=99).contains(target))
        .map(|target| target + 44)
        .unwrap_or_else(|| panic!("unknown -jvm-target {jvm_target}"));
    let classes = common::compile_in_process_metadata_cp_module_target(
        src,
        name,
        cp_jars,
        "main",
        Some(class_major),
    )
    .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(path, bytes).ok()?;
    }

    let reference = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &reference_dir.to_string_lossy(),
        class,
    ])?;
    let krusty = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &krusty_dir.to_string_lossy(),
        class,
    ])?;
    let reference_bytes = std::fs::read(reference_dir.join(format!("{class}.class"))).ok()?;
    let krusty_bytes = std::fs::read(krusty_dir.join(format!("{class}.class"))).ok()?;
    let _ = std::fs::remove_dir_all(dir);
    Some(ReferenceComparison {
        reference,
        krusty,
        reference_bytes,
        krusty_bytes,
    })
}

/// The serialization runtime the generated code links against, plus the plugin jar kotlinc needs in
/// order to produce the reference at all. `None` when either is absent from the local caches.
fn plugin_and_runtime() -> Option<(PathBuf, Vec<PathBuf>)> {
    let plugin = kotlinc_plugin_jar("kotlinx-serialization-compiler-plugin")?;
    let core = gradle_module_jar("org.jetbrains.kotlinx", "kotlinx-serialization-core-jvm")?;
    Some((plugin, vec![core, common::stdlib_jar()]))
}

/// javap output reduced to what this test asserts: member signatures, flags, instructions, debug
/// tables, and the `InnerClasses` table, with constant-pool indices erased because their numbering
/// is an emission-order artifact.
fn structure(disassembly: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in disassembly.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        // The constant pool is an emission-order artifact: same entries, different numbering, and
        // the whole point of a structural comparison is not to depend on it.
        if trimmed.starts_with("Constant pool:") || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.is_empty()
            || trimmed.starts_with("Classfile ")
            || trimmed.starts_with("SHA-256")
            || trimmed.starts_with("Last modified")
            || trimmed.starts_with("Compiled from")
        {
            continue;
        }
        // `#21` / `#21,  2` are pool indices: identical structure, different numbering.
        let mut normalized = String::with_capacity(trimmed.len());
        let mut chars = trimmed.chars().peekable();
        while let Some(c) = chars.next() {
            normalized.push(c);
            if c == '#' {
                while chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                    chars.next();
                }
            }
        }
        out.push(normalized.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    out
}

/// A `@Serializable` enum's `$cachedSerializer$delegate` sits directly after `Companion` and BEFORE
/// the entry constants — the same leading block the companion field is in, not the tail after
/// `$VALUES`/`$ENTRIES`. Asserted as an ORDER because the enum class is not yet byte-identical (it
/// still lacks the `invokedynamic` `Lazy` initializer and the field's `Signature`), so a byte
/// assertion here would fail on those instead of on the ordering under test.
#[test]
fn a_serializable_enum_places_its_delegate_next_to_the_companion() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               enum class Status { ACTIVE, INACTIVE }\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializableEnumFields", src, "Status", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Field NAMES in emission order. `javap -v` prints plenty that also ends in `;` — constant-pool
    // `Utf8` rows, `Signature:` attributes, `LocalVariableTable` rows — so anchor on a leading
    // access modifier, and compare names rather than whole declarations: the delegate's rendered
    // type still differs (krusty has no field `Signature` yet), which is a separate gap from the
    // ORDER under test here.
    let declarations = |text: &str| {
        text.lines()
            .map(str::trim)
            .filter(|line| {
                line.ends_with(';')
                    && !line.contains('(')
                    && !line.contains('{')
                    && ["public ", "private ", "protected "]
                        .iter()
                        .any(|lead| line.starts_with(lead))
            })
            .filter_map(|line| line.split_whitespace().last())
            .map(|name| name.trim_end_matches(';').to_string())
            .collect::<Vec<_>>()
    };
    let want = declarations(&built.reference);
    assert!(
        want.iter()
            .take(2)
            .any(|line| line.contains("$cachedSerializer$delegate")),
        "reference must place the delegate in the leading block:\n{}",
        built.reference
    );
    assert_eq!(declarations(&built.krusty), want, "enum field order");
}

/// A `@Serializable` ENUM's companion. kotlinc does not inline the cached-serializer lookup into
/// `serializer()`: it puts that body in a private, SYNTHETIC `get$cachedSerializer()` and has
/// `serializer()` delegate to it with `invokespecial`. krusty inlined the lookup, so the companion
/// had one method where kotlinc has two.
///
/// Being synthetic is what carries the rest of the shape: `ACC_SYNTHETIC`, exclusion from
/// `@Metadata` (a compiler-invented member is not a declaration reflection should see), and — since
/// the helper is created in the backend, past the frontend's generated-declaration line transfer —
/// a declaration line copied from the companion so the debug tables get attached at all.
///
/// This asserts full BYTE equality rather than structure: every one of those facts has to hold at
/// once for the class to match, and they are individually easy to get right while still differing.
#[test]
fn serializable_enum_companion_is_byte_identical() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               enum class Status { ACTIVE, INACTIVE }\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializableEnumCompanion",
        src,
        "Status$Companion",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert!(
        built.reference.contains("get$cachedSerializer"),
        "reference must carry the helper this test is about:\n{}",
        built.reference
    );
    if built.krusty_bytes != built.reference_bytes {
        let (want, got) = (structure(&built.reference), structure(&built.krusty));
        let first = want
            .iter()
            .zip(got.iter())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| want.len().min(got.len()));
        panic!(
            "Status$Companion differs from kotlinc ({} B vs {} B); first structural difference at line {first}\n  kotlinc: {:?}\n  krusty:  {:?}",
            built.krusty_bytes.len(),
            built.reference_bytes.len(),
            want.get(first),
            got.get(first),
        );
    }
}

#[test]
fn serializable_companion_matches_kotlinc_structure() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializableCompanion",
        SRC,
        "Point$Companion",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let (reference, krusty) = (built.reference.as_str(), built.krusty.as_str());
    let (want, got) = (structure(reference), structure(krusty));
    assert_eq!(
        want.iter().filter(|l| l.contains("checkcast")).count(),
        1,
        "reference must contain the checkcast this test is about:\n{reference}"
    );
    assert!(
        want.iter().any(|l| l.contains("InnerClasses")),
        "reference must carry an InnerClasses table:\n{reference}"
    );
    if want != got {
        let first = want
            .iter()
            .zip(got.iter())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| want.len().min(got.len()));
        panic!(
            "Point$Companion structure differs from kotlinc at line {first}\n  kotlinc: {:?}\n  krusty:  {:?}\n\nfull kotlinc:\n{}\n\nfull krusty:\n{}",
            want.get(first),
            got.get(first),
            want.join("\n"),
            got.join("\n"),
        );
    }

    // javap does not render `ACC_SYNTHETIC` in the `InnerClasses` table, so the disassembly above
    // is blind to the second half of this fix — compare the parsed entries.
    let want_inner = inner_classes(&built.reference_bytes);
    let got_inner = inner_classes(&built.krusty_bytes);
    assert!(
        want_inner
            .iter()
            .any(|(inner, access)| inner.ends_with("$$serializer") && access & 0x1000 != 0),
        "reference must mark the generated $serializer synthetic in InnerClasses: {want_inner:?}"
    );
    assert_eq!(got_inner, want_inner, "InnerClasses entries");
}

/// The class's `InnerClasses` entries as `(inner internal name, access flags)`.
fn inner_classes(bytes: &[u8]) -> Vec<(String, u16)> {
    krusty::jvm::classreader::parse_class(bytes)
        .expect("emitted class parses")
        .inner_classes
        .iter()
        .map(|entry| (entry.inner.clone(), entry.access))
        .collect()
}
