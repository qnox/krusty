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

use krusty::types::Ty;

use super::common;
use super::common_core;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Point(val x: Int, val y: String)\n";

/// The newest `<artifact>-<version>.jar` Gradle cached for one exact module coordinate.
pub(super) fn gradle_module_jar(group: &str, artifact: &str) -> Option<PathBuf> {
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

pub(super) struct ReferenceComparison {
    pub(super) reference: String,
    pub(super) krusty: String,
    reference_bytes: Vec<u8>,
    krusty_bytes: Vec<u8>,
}

/// Build one class with the reference serialization plugin and with krusty.
pub(super) fn compare_with_kotlinc_plugin(
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

/// Multi-file form of [`compare_with_kotlinc_plugin`], used for module facts that cannot be tested
/// by compiling declarations in one source unit.
fn compare_files_with_kotlinc_plugin(
    sources: &[(&str, &str)],
    class: &str,
    cp_jars: &[PathBuf],
    kotlinc_extra: &[String],
) -> Option<ReferenceComparison> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&krusty_dir).ok()?;
    let mut source_paths = Vec::with_capacity(sources.len());
    for (name, source) in sources {
        let path = dir.join(name);
        std::fs::write(&path, source).ok()?;
        source_paths.push(path);
    }

    let mut arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-classpath".to_string(),
        cp_jars
            .iter()
            .map(|jar| jar.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":"),
    ];
    arguments.extend(kotlinc_extra.iter().cloned());
    arguments.extend(
        source_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    let (code, stderr) = common::kotlinc_compile(&arguments)?;
    assert_eq!(code, 0, "kotlinc(source set) failed: {stderr}");

    let classes =
        common::compile_in_process_files(sources, cp_jars, Some(common::jdk_modules().as_path()))?;
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(path, bytes).ok()?;
    }
    let display_class = class.replace('/', ".");
    let reference = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &reference_dir.to_string_lossy(),
        &display_class,
    ])?;
    let krusty = common::javap(&[
        "-p",
        "-c",
        "-v",
        "-cp",
        &krusty_dir.to_string_lossy(),
        &display_class,
    ])?;
    let reference_bytes = std::fs::read(reference_dir.join(format!("{class}.class"))).ok()?;
    let krusty_bytes = std::fs::read(krusty_dir.join(format!("{class}.class"))).ok()?;
    std::fs::remove_dir_all(dir).ok()?;
    Some(ReferenceComparison {
        reference,
        krusty,
        reference_bytes,
        krusty_bytes,
    })
}

/// The serialization runtime the generated code links against, plus the plugin jar kotlinc needs in
/// order to produce the reference at all. The runtime uses the repository's pinned dependency
/// provisioner instead of depending on an unrelated Gradle build having populated its private
/// cache first.
pub(super) fn plugin_and_runtime() -> Option<(PathBuf, Vec<PathBuf>)> {
    let plugin = kotlinc_plugin_jar("kotlinx-serialization-compiler-plugin")?;
    let core = krusty::toolchain::serialization_core_jar()?;
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

/// An enum whose declaration spans lines: the annotation on one, the header on the next, and a
/// constructor property. kotlinc maps the constructor's `super(name, ordinal)` to where the
/// DECLARATION starts and the property stores that follow to the HEADER line — two entries. krusty
/// emitted one, at the stores' pc.
///
/// Its `<clinit>` also returns on the closing-brace line. A single-line enum cannot show either
/// distinction because all source lines coincide.
#[test]
fn an_annotated_enum_maps_constructor_and_clinit_lines() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               \n\
               @Serializable\n\
               enum class Phase(val value: String) {\n\
               \x20   UNKNOWN(\"unknown\"),\n\
               \x20   PENDING(\"pending\"),\n\
               }\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializableEnumCtorLines", src, "Phase", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let ctor_lines = |text: &str| {
        let rows: Vec<&str> = text.lines().map(str::trim).collect();
        let at = rows
            .iter()
            .position(|line| {
                line.starts_with("private Phase(") || line.contains(" Phase(java.lang.String);")
            })
            .expect("enum constructor");
        rows.into_iter()
            .skip(at)
            .skip_while(|line| !line.starts_with("LineNumberTable"))
            .skip(1)
            .take_while(|line| line.starts_with("line "))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let want = ctor_lines(&built.reference);
    assert_eq!(
        want.len(),
        2,
        "reference maps super() and the stores separately: {want:?}\n{}",
        built.reference
    );
    assert_eq!(
        ctor_lines(&built.krusty),
        want,
        "constructor LineNumberTable"
    );

    let clinit_lines = |text: &str| {
        let rows: Vec<&str> = text.lines().map(str::trim).collect();
        let at = rows
            .iter()
            .position(|line| *line == "static {};")
            .expect("enum has <clinit>");
        rows.into_iter()
            .skip(at)
            .skip_while(|line| !line.starts_with("LineNumberTable"))
            .skip(1)
            .take_while(|line| line.starts_with("line "))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let want = clinit_lines(&built.reference);
    let closing_line = want.last().and_then(|entry| {
        entry
            .strip_prefix("line ")?
            .split_once(':')?
            .0
            .parse::<u32>()
            .ok()
    });
    assert_eq!(
        closing_line,
        Some(7),
        "reference returns on the closing brace"
    );
    assert_eq!(
        clinit_lines(&built.krusty),
        want,
        "<clinit> LineNumberTable"
    );
}

/// kotlinc's `$serializer` member order is `<init>`, `serialize`, `deserialize`, `getDescriptor`,
/// `childSerializers`, `typeParametersSerializers`, then the bridges. krusty emitted `getDescriptor`
/// second, because the plugin DECLARES it first — the descriptor field it returns is built in
/// `<init>` — and the class's member list was written in declaration order.
#[test]
fn a_generated_serializer_emits_get_descriptor_after_deserialize() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Point(val x: Int, val y: String)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializerOrder",
        src,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Member names in emission order. Anchor on a leading access modifier: `javap -v` also prints
    // constant-pool rows and `LocalVariableTable` entries that end in `;` and carry no `(`, and an
    // LVT row's last token is a descriptor that reads exactly like a member name here.
    let members = |text: &str| {
        text.lines()
            .map(str::trim)
            .filter(|line| {
                line.ends_with(';')
                    && ["public ", "private ", "protected "]
                        .iter()
                        .any(|lead| line.starts_with(lead))
            })
            .filter_map(|line| line.split('(').next()?.split_whitespace().last())
            .map(|name| name.trim_end_matches(';').to_string())
            .collect::<Vec<_>>()
    };
    let want = members(&built.reference);
    let at = |names: &[String], name: &str| names.iter().position(|n| n == name);
    assert!(
        matches!((at(&want, "getDescriptor"), at(&want, "deserialize")), (Some(g), Some(d)) if g > d),
        "reference must emit getDescriptor after deserialize: {want:?}"
    );
    assert_eq!(members(&built.krusty), want, "$serializer member order");
}

/// The whole `@Serializable` enum CLASS, byte for byte. Everything above builds to this: the lazy
/// `invokedynamic` initializer, the member and field order, the delegate's attributes, the debug
/// tables, the pool order, and the class-attribute order.
#[test]
fn a_serializable_enum_class_is_byte_identical() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               enum class Status { ACTIVE, INACTIVE }\n";
    for class in ["Status", "Status$Companion"] {
        let Some(built) =
            compare_with_kotlinc_plugin("SerializableEnumBytes", src, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        if built.krusty_bytes != built.reference_bytes {
            let (want, got) = (structure(&built.reference), structure(&built.krusty));
            let first = want
                .iter()
                .zip(got.iter())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| want.len().min(got.len()));
            panic!(
                "{class} differs from kotlinc ({} B vs {} B); first structural difference at line {first}\n  kotlinc: {:?}\n  krusty:  {:?}",
                built.krusty_bytes.len(),
                built.reference_bytes.len(),
                want.get(first),
                got.get(first),
            );
        }
    }
}

/// A user annotation on an enum interns with the CLASS-ATTRIBUTE window, immediately before
/// `@Metadata` — kotlinc's order. krusty queued it at the top of the emit, which put
/// `Lkotlinx/serialization/Serializable;` near the head of the constant pool and shifted everything
/// after it.
#[test]
fn a_serializable_enums_annotation_interns_with_the_class_attributes() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               enum class Status { ACTIVE, INACTIVE }\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializableEnumAnnPool", src, "Status", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Where the annotation descriptor lands, as a FRACTION of the pool — the absolute index still
    // differs further down, but kotlinc puts it in the last quarter and krusty put it in the first.
    let position = |text: &str| {
        let rows: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('#') && line.contains(" = "))
            .collect();
        let at = rows
            .iter()
            .position(|line| line.contains("Lkotlinx/serialization/Serializable;"))?;
        Some((at, rows.len()))
    };
    let (want_at, want_len) = position(&built.reference).expect("reference interns the annotation");
    let (got_at, got_len) = position(&built.krusty).expect("krusty interns the annotation");
    assert!(
        want_at * 4 > want_len * 3,
        "reference should intern it in the last quarter: {want_at} of {want_len}"
    );
    assert!(
        got_at * 4 > got_len * 3,
        "krusty interns the annotation at {got_at} of {got_len}; kotlinc at {want_at} of {want_len}"
    );
}

/// `<clinit>`'s LineNumberTable steps back to the ANNOTATION line for the generated delegate's
/// store, then returns to the entries' line (`0:6, 55:5, 69:6`). krusty emitted a single `0:6`.
///
/// Marking the store on the builder does NOT reach the attribute: `add_method` DROPS a
/// `<clinit>`/`<init>` builder's line marks because those tables are curated afterwards through
/// `set_method_lines`. The entry has to be pushed into that curated list.
#[test]
fn a_serializable_enum_maps_its_delegate_store_to_the_annotation_line() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               enum class Status { ACTIVE, INACTIVE }\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializableEnumClinit", src, "Status", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // The `line N: pc` rows of the LAST method javap prints — `<clinit>` is emitted last.
    let clinit_lines = |text: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .rposition(|line| line.contains("static {}"))
            .unwrap_or(0);
        body.into_iter()
            .skip(start)
            .filter(|line| line.starts_with("line "))
            .collect::<Vec<_>>()
    };
    let want = clinit_lines(&built.reference);
    assert!(
        want.len() > 1,
        "reference must map more than one line in <clinit>:\n{}",
        built.reference
    );
    assert_eq!(
        clinit_lines(&built.krusty),
        want,
        "<clinit> LineNumberTable"
    );
}

/// kotlinc does not build a `@Serializable` enum's cached serializer eagerly. It compiles the
/// initializer to a private synthetic `_init_$_anonymous_()` calling the static factory
/// `EnumsKt.createSimpleEnumSerializer(name, values())`, binds that with an `invokedynamic`
/// `Function0`, and hands it to `LazyKt.lazy(LazyThreadSafetyMode.PUBLICATION, …)` — so the
/// serializer is constructed on first use and the class carries `BootstrapMethods`.
///
/// krusty built it eagerly with `LazyKt.lazyOf(new EnumSerializer(…))`: no indy, no bootstrap
/// methods, no helper, and the serializer allocated during `<clinit>`.
///
/// Note the explicit `checkcast [Ljava/lang/Enum;` on `values()`. The JVM does not need it — arrays
/// are covariant — but kotlinc emits it, so byte parity does too.
#[test]
fn a_serializable_enum_builds_its_serializer_lazily() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               enum class Status { ACTIVE, INACTIVE }\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializableEnumLazy", src, "Status", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let helper_signature = |text: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains("_init_$_anonymous_()"))
            .expect("initializer helper declaration");
        body.into_iter().skip(start).take(3).collect::<Vec<_>>()
    };
    let want_signature = helper_signature(&built.reference);
    assert_eq!(
        want_signature.get(2).map(String::as_str),
        Some("flags: (0x101a) ACC_PRIVATE, ACC_STATIC, ACC_FINAL, ACC_SYNTHETIC"),
        "reference initializer helper flags"
    );
    assert_eq!(
        helper_signature(&built.krusty),
        want_signature,
        "_init_$_anonymous_ declaration, descriptor, and flags"
    );
    // The helper's own instruction sequence, pool indices erased.
    let initializer = |text: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains("_init_$_anonymous_()"))
            .unwrap_or(body.len());
        body.into_iter()
            .skip(start)
            .take_while(|line| !line.contains("access$get$cachedSerializer"))
            // Instruction rows only (`8: invokestatic …`). `descriptor:`, `flags:` and the debug
            // tables also carry a colon, and the tables are a separate gap from the body shape.
            .filter(|line| {
                line.split_once(": ").is_some_and(|(offset, _)| {
                    !offset.is_empty() && offset.bytes().all(|b| b.is_ascii_digit())
                })
            })
            .collect::<Vec<_>>()
    };
    let want = initializer(&built.reference);
    assert!(
        want.iter()
            .any(|line| line.contains("createSimpleEnumSerializer")),
        "reference must build through the static factory:\n{}",
        built.reference
    );
    assert!(
        want.iter().any(|line| line.contains("checkcast")),
        "reference must narrow values() to [Ljava/lang/Enum;:\n{}",
        built.reference
    );
    assert_eq!(initializer(&built.krusty), want, "_init_$_anonymous_ body");

    // An enum initializes its companion before the lazy delegate that may read through it. Compare
    // the complete `<clinit>` instruction stream: checking only that both stores exist would allow
    // the old reversed order to return.
    let clinit_instructions = |text: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line == "static {};")
            .expect("class initializer declaration");
        body.into_iter()
            .skip(start)
            .take_while(|line| line != "}")
            .filter(|line| {
                line.split_once(": ").is_some_and(|(offset, _)| {
                    !offset.is_empty() && offset.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
            .collect::<Vec<_>>()
    };
    let want_clinit = clinit_instructions(&built.reference);
    let companion_store = want_clinit
        .iter()
        .position(|line| line.contains("Field Companion:"));
    let delegate_store = want_clinit
        .iter()
        .position(|line| line.contains("Field $cachedSerializer$delegate:"));
    assert!(
        matches!((companion_store, delegate_store), (Some(companion), Some(delegate)) if companion < delegate),
        "reference must initialize Companion before the serializer delegate:\n{}",
        built.reference
    );
    assert_eq!(
        clinit_instructions(&built.krusty),
        want_clinit,
        "<clinit> instruction order"
    );

    // These members are generated after the frontend line handoff. Their exact source line is the
    // annotated declaration's START, not the later `enum class` keyword and not a fallback line.
    let source_lines = |text: &str, start_marker: &str, end_marker: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains(start_marker))
            .unwrap_or(body.len());
        body.into_iter()
            .skip(start)
            .take_while(|line| !line.contains(end_marker))
            .filter(|line| line.starts_with("line "))
            .collect::<Vec<_>>()
    };
    for (start, end) in [
        ("_init_$_anonymous_()", "access$get$cachedSerializer"),
        ("access$get$cachedSerializer", "static {};"),
    ] {
        let want_lines = source_lines(&built.reference, start, end);
        assert_eq!(
            want_lines,
            vec!["line 2: 0"],
            "reference {start} line must be the @Serializable declaration start"
        );
        assert_eq!(
            source_lines(&built.krusty, start, end),
            want_lines,
            "{start} LineNumberTable"
        );
    }
    for marker in ["BootstrapMethods", "LazyThreadSafetyMode", "invokedynamic"] {
        assert!(
            built.krusty.contains(marker),
            "krusty must emit {marker}:\n{}",
            built.krusty
        );
    }
}

/// A `@Serializable` enum's `$cachedSerializer$delegate` sits directly after `Companion` and BEFORE
/// the entry constants — the same leading block the companion field is in, not the tail after
/// `$VALUES`/`$ENTRIES`. Its complete declaration carries the same generic `Signature` and `@NotNull`
/// as kotlinc, while the generated raw-`Lazy` access bridge carries neither attribute.
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
    // Field names in emission order remain an independent contract: comparing only the delegate's
    // attributes would not catch it drifting back behind the enum entries.
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

    let member_block = |text: &str, start: &str, end: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.ends_with(start))
            .expect("member declaration");
        body.into_iter()
            .skip(start)
            .take_while(|line| !line.ends_with(end))
            .collect::<Vec<_>>()
    };
    let reference_delegate =
        member_block(&built.reference, "$cachedSerializer$delegate;", "ACTIVE;");
    assert!(
        reference_delegate.iter().any(|line| {
            line.contains("Lkotlin/Lazy<Lkotlinx/serialization/KSerializer<Ljava/lang/Object;>;>;")
        }),
        "reference delegate must carry its complete generic Signature: {reference_delegate:?}"
    );
    assert!(
        reference_delegate
            .iter()
            .any(|line| line.contains("org.jetbrains.annotations.NotNull")),
        "reference delegate must carry @NotNull: {reference_delegate:?}"
    );
    assert_eq!(
        member_block(&built.krusty, "$cachedSerializer$delegate;", "ACTIVE;",),
        reference_delegate,
        "delegate declaration and attributes"
    );

    let reference_bridge = member_block(
        &built.reference,
        "access$get$cachedSerializer$delegate$cp();",
        "static {};",
    );
    assert!(
        reference_bridge.iter().all(|line| {
            !line.starts_with("Signature:")
                && !line.contains("org.jetbrains.annotations.NotNull")
                && !line.contains("org.jetbrains.annotations.Nullable")
        }),
        "reference raw-Lazy bridge must carry neither a Signature nor nullability: {reference_bridge:?}"
    );
    assert_eq!(
        member_block(
            &built.krusty,
            "access$get$cachedSerializer$delegate$cp();",
            "static {};",
        ),
        reference_bridge,
        "raw-Lazy bridge declaration and attributes"
    );
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

/// A `@Serializable` declaration's SERIAL NAME is its qualified Kotlin name, with the package
/// separator AND the nesting separator both written as dots. krusty built it from the JVM internal
/// name and replaced only `/`, so a nested declaration got `Outer$Middle$Phase` where kotlinc writes
/// `Outer.Middle.Phase` — the serial form a peer decoder reads, and invisible to any top-level
/// fixture.
///
/// Both generated carriers are checked: an enum's `createSimpleEnumSerializer(<name>, …)` and a
/// class's `PluginGeneratedSerialDescriptor(<name>, …)`. A backticked nested name also proves that
/// a declared `$` is preserved rather than mistaken for another nesting boundary.
#[test]
fn a_nested_serializable_declaration_spells_its_serial_name_with_dots() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               class Outer {\n\
               \x20   class Middle {\n\
               \x20       @Serializable\n\
               \x20       enum class Phase(val value: String) { UNKNOWN(\"unknown\") }\n\
               \x20\n\
               \x20       @Serializable\n\
               \x20       data class Point(val x: Int)\n\
               \x20\n\
               \x20       @Serializable\n\
               \x20       data class `Dollar$Point`(val x: Int)\n\
               \x20   }\n\
               }\n";
    // The enum carries its own serial name; a class's lives on the generated `$serializer`.
    for (class, expected) in [
        ("Outer$Middle$Phase", "Outer.Middle.Phase"),
        ("Outer$Middle$Point$$serializer", "Outer.Middle.Point"),
        (
            "Outer$Middle$Dollar$Point$$serializer",
            "Outer.Middle.Dollar$Point",
        ),
    ] {
        let Some(built) =
            compare_with_kotlinc_plugin("NestedSerialName", src, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        // The string PAYLOADS only: a generated body's instruction offsets are a separate parity
        // question, and pinning them here would fail this test for something it does not test.
        let names = |disassembly: &str| {
            disassembly
                .lines()
                .filter_map(|line| line.split_once("// String Outer"))
                .map(|(_, name)| format!("Outer{}", name.trim()))
                .collect::<Vec<_>>()
        };
        let want = names(&built.reference);
        assert!(
            want.iter().any(|line| line.ends_with(expected)),
            "reference must load the dotted serial name {expected} — the rule under test:\n{}",
            built.reference
        );
        assert_eq!(names(&built.krusty), want, "{class}: serial name constants");
    }
}

/// An entry's `@SerialName` is the name the FORMAT reads and writes for that constant, and it
/// selects a different factory: `createSimpleEnumSerializer` derives every name from the constant's
/// own spelling, so `@SerialName("active") ACTIVE` serialized as `"ACTIVE"` — wrong data, not a byte
/// difference. kotlinc emits `createAnnotatedEnumSerializer(name, values(), names, entryAnnotations,
/// classAnnotations)`, passing `null` for an entry that carries no name of its own.
///
/// The annotation lives on the entry's static FIELD — an enum constant has no property — which is
/// why a property-driven `@SerialName` lookup never saw it.
#[test]
fn an_enum_entrys_serial_name_reaches_its_serializer() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = SERIAL_NAME_ENUM;
    let Some(built) =
        compare_with_kotlinc_plugin("EnumSerialName", src, "Status", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // The generated initializer's body: the factory call and the arrays it builds.
    let initializer = |text: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains("_init_$_anonymous_"))
            .expect("generated enum serializer initializer is present");
        body.into_iter()
            .skip(start)
            .take_while(|line| !line.starts_with("public") && !line.starts_with("private static ["))
            .collect::<Vec<_>>()
    };
    let want = initializer(&built.reference);
    assert!(
        want.iter()
            .any(|line| line.contains("createAnnotatedEnumSerializer")),
        "reference must use the annotated factory — the rule under test:\n{}",
        built.reference
    );
    assert_eq!(
        initializer(&built.krusty),
        want,
        "generated enum-serializer initializer"
    );
}

/// The names must reach the DESCRIPTOR, which is what a format actually reads. This is the
/// assertion the byte comparison cannot make: a serializer built from the wrong factory verifies,
/// runs, and produces different data.
#[test]
fn an_enum_serializer_reports_the_entrys_serial_name() {
    let Some((_, cp)) = plugin_and_runtime() else {
        return;
    };
    let src = format!(
        "{SERIAL_NAME_ENUM}\
         fun box(): String {{\n\
         \x20 val descriptor = Status.serializer().descriptor\n\
         \x20 return descriptor.getElementName(0) + \",\" + descriptor.getElementName(1) + \",\" + descriptor.getElementName(2)\n\
         }}\n"
    );
    let jdk = common::jdk_modules();
    let Some(classes) =
        common::compile_in_process(&src, "EnumSerialNameRun", &cp, Some(jdk.as_path()))
    else {
        panic!(
            "{:?}",
            common::front_end_diagnostics(&src, &cp, Some(jdk.as_path()))
        );
    };
    assert_eq!(
        common::run_box(&classes, "EnumSerialNameRunKt", &cp).expect("box runner"),
        "active,warning,PLAIN"
    );
}

/// Two annotated entries prove order; the trailing plain entry proves the factory receives a null
/// slot and falls back to the constant spelling only for that entry.
const SERIAL_NAME_ENUM: &str = "import kotlinx.serialization.SerialName\n\
                                import kotlinx.serialization.Serializable\n\
                                @Serializable\n\
                                enum class Status(val value: String) {\n\
                                \x20   @SerialName(\"active\")\n\
                                \x20   ACTIVE(\"active\"),\n\
                                \x20\n\
                                \x20   @SerialName(\"warning\")\n\
                                \x20   WARNING(\"warning\"),\n\
                                \x20\n\
                                \x20   PLAIN(\"plain\"),\n\
                                }\n";

/// An enum CONSTANT's annotation type interns in the field-table window — beside the deferred
/// fields' `Signature` strings and just before the class's own annotations — not where the
/// constant's field was added.
///
/// The entry fields cannot themselves be deferred: their `<clinit>` `putstatic` references
/// interleave with the entry names, which is an order kotlinc also produces. Only the ANNOTATION
/// encoding moves. Encoding it eagerly put `Lkotlinx/serialization/SerialName;` right after the
/// first entry name and shifted every later pool index — the class matched kotlinc in every other
/// respect and still differed byte-wise.
#[test]
fn an_enum_entrys_annotation_type_interns_with_the_field_table() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "EnumEntryAnnotationPool",
        SERIAL_NAME_ENUM,
        "Status",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Where the entry annotation's type lands, as a position in the pool.
    let position = |text: &str| {
        let rows: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('#') && line.contains(" = "))
            .collect();
        let at = rows
            .iter()
            .position(|line| line.contains("Lkotlinx/serialization/SerialName;"))
            .unwrap_or_else(|| panic!("no SerialName descriptor in the pool:\n{text}"));
        (at, rows.len())
    };
    let (want_at, want_len) = position(&built.reference);
    let (got_at, got_len) = position(&built.krusty);
    assert!(
        want_at * 4 > want_len * 3,
        "reference should intern it in the last quarter: {want_at} of {want_len}"
    );
    assert_eq!(
        (got_at, got_len),
        (want_at, want_len),
        "entry-annotation descriptor position in the constant pool"
    );
}

/// With the entry's serial name in its `@Metadata` and its annotation type interned in the field
/// window, a `@Serializable` enum whose constants carry `@SerialName` is byte-identical to kotlinc.
///
/// This is the assertion the individual ones cannot make: each of the three facts — the factory
/// call, the metadata record, the pool position — leaves the class differing on its own, so only
/// the whole-class comparison shows the shape is finished.
#[test]
fn a_serializable_enum_with_entry_serial_names_is_byte_identical() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "EnumSerialNameBytes",
        SERIAL_NAME_ENUM,
        "Status",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    if built.krusty_bytes != built.reference_bytes {
        let at = built
            .krusty_bytes
            .iter()
            .zip(&built.reference_bytes)
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| built.krusty_bytes.len().min(built.reference_bytes.len()));
        let (want, got) = (structure(&built.reference), structure(&built.krusty));
        let first = want
            .iter()
            .zip(got.iter())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| want.len().min(got.len()));
        panic!(
            "bytes differ at offset {at} (krusty {} B, kotlinc {} B)\n  kotlinc: {:?}\n  krusty:  {:?}",
            built.krusty_bytes.len(),
            built.reference_bytes.len(),
            want.get(first),
            got.get(first),
        );
    }
}

/// kotlinc marks every generated `$serializer` `@Deprecated(…, level = HIDDEN)`. The class is an
/// implementation detail of the plugin, and the HIDDEN level is what keeps it out of a consumer's
/// resolution while its realization stays callable — a semantic fact a consumer reads, not a
/// cosmetic attribute: without it the class is an ordinary public API to every other compiler.
#[test]
fn a_generated_serializer_carries_the_hidden_deprecated_marker() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializerDeprecated",
        SRC,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // The rendered `kotlin.Deprecated(...)` block javap prints for the CLASS.
    let marker = |text: &str| {
        let rows: Vec<&str> = text.lines().map(str::trim).collect();
        let at = rows
            .iter()
            .position(|line| line.starts_with("kotlin.Deprecated("))?;
        // Stop at the marker's own closing paren: the next annotation's raw constant-pool row
        // follows immediately, and its indices are an emission-order artifact.
        let mut block = Vec::new();
        for line in &rows[at..] {
            block.push((*line).to_string());
            if *line == ")" {
                break;
            }
        }
        Some(block)
    };
    let want = marker(&built.reference).unwrap_or_else(|| {
        panic!(
            "reference must mark the generated serializer deprecated:\n{}",
            built.reference
        )
    });
    assert!(
        want.iter().any(|line| line.contains("HIDDEN")),
        "the reference marker must carry the HIDDEN level — the rule under test: {want:?}"
    );
    let annotation_section = |text: &str| {
        let rows = text.lines().map(str::trim).collect::<Vec<_>>();
        let marker = rows
            .iter()
            .position(|line| line.starts_with("kotlin.Deprecated("))?;
        rows[..marker]
            .iter()
            .rfind(|line| line.ends_with("Annotations:"))
            .map(|line| (*line).to_string())
    };
    assert_eq!(
        annotation_section(&built.reference),
        Some("RuntimeVisibleAnnotations:".to_string()),
        "kotlinc emits the marker with Kotlin Deprecated's runtime retention"
    );
    assert_eq!(
        marker(&built.krusty),
        Some(want),
        "@Deprecated marker on the generated serializer"
    );
    assert_eq!(
        annotation_section(&built.krusty),
        annotation_section(&built.reference),
        "@Deprecated retention section on the generated serializer"
    );

    // Kotlin consumers also read the annotation from the class metadata. Compare the exact d2
    // string table rendered inside `kotlin.Metadata`, not merely the standalone JVM annotation.
    let metadata_marker = |text: &str| {
        let d2 = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("d2=["))?;
        let marker = d2.find("\"Lkotlin/Deprecated;\"")?;
        Some(d2[marker..].to_string())
    };
    let reference_marker =
        metadata_marker(&built.reference).expect("reference metadata carries the marker");
    assert!(
        [
            "Lkotlin/Deprecated;",
            "This synthesized declaration should not be used directly",
            "Lkotlin/DeprecationLevel;",
            "HIDDEN",
        ]
        .iter()
        .all(|entry| reference_marker.contains(entry)),
        "reference metadata must retain the complete marker: {reference_marker}"
    );
    assert_eq!(
        metadata_marker(&built.krusty),
        Some(reference_marker),
        "deprecated-marker suffix in the generated serializer's @Metadata d2"
    );
}

/// A generated `$serializer` always carries a class `Signature`: even with no type parameters of its
/// own, the interface it implements is generic (`GeneratedSerializer<Point>`), and that
/// instantiation exists only in the signature — the descriptor erases it.
///
/// krusty wrote the attribute only for a GENERIC serializer, and wrote the wrong supertype there:
/// `KSerializer<Box<T>>` (the semantic supertype) instead of the `GeneratedSerializer<Box<T>>` the
/// class actually implements. Both shapes are checked because they failed differently — one was
/// missing the attribute, the other had it with the wrong interface.
#[test]
fn a_generated_serializer_carries_its_class_signature() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let generic = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Box<T>(val value: T, val tag: String)\n";
    for (name, src, class) in [
        ("SerializerSignature", SRC, "Point$$serializer"),
        ("SerializerSignatureGeneric", generic, "Box$$serializer"),
    ] {
        let Some(built) = compare_with_kotlinc_plugin(name, src, class, &cp, "25", &extra) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        // The CLASS `Signature` javap prints — the one at column 0. A MEMBER's signature is
        // indented under its declaration, and `structure` trims indentation, so the raw text is
        // what distinguishes them. Pool indices are erased; their numbering is emission order.
        let signature = |text: &str| {
            text.lines()
                .find(|line| line.starts_with("Signature: "))
                .map(|line| {
                    line.split_once("// ")
                        .map_or_else(|| line.to_string(), |(_, value)| value.to_string())
                })
        };
        let want = signature(&built.reference).unwrap_or_else(|| {
            panic!(
                "{class}: reference must carry a class Signature:\n{}",
                built.reference
            )
        });
        assert!(
            want.contains("GeneratedSerializer<"),
            "{class}: the reference signature must name the implemented interface: {want}"
        );
        assert_eq!(
            signature(&built.krusty),
            Some(want),
            "{class}: class Signature"
        );
    }
}

/// A generated `$serializer`'s members are public API — a Java caller can pass `null` — so kotlinc
/// guards their non-null reference parameters at entry exactly as it guards a user-written
/// function: `Intrinsics.checkNotNullParameter(encoder, "encoder")`. krusty emitted the body with
/// no prologue at all, so every `serialize`/`deserialize` differed from the first instruction on.
///
/// The generated-parameter record also pins the names in Kotlin Metadata to
/// `encoder`/`value`/`decoder` rather than positional placeholders.
#[test]
fn a_generated_serializer_guards_its_parameters() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializerParamGuards",
        SRC,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Read the names through stable neighboring type entries. Member order and unrelated parts of
    // kotlinc's string table still differ, so comparing all of `d2` would over-couple this test.
    let metadata_parameter_names = |text: &str| {
        let d2 = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("d2=["))
            .unwrap_or_else(|| panic!("generated serializer metadata string table:\n{text}"));
        let strings = d2.split('"').skip(1).step_by(2).collect::<Vec<_>>();
        let encoder_type = strings
            .iter()
            .position(|value| *value == "Lkotlinx/serialization/encoding/Encoder;")
            .expect("Encoder metadata type");
        let decoder_type = strings
            .iter()
            .position(|value| *value == "Lkotlinx/serialization/encoding/Decoder;")
            .expect("Decoder metadata type");
        vec![
            strings[encoder_type - 1].to_string(),
            strings[encoder_type + 1].to_string(),
            strings[decoder_type - 1].to_string(),
        ]
    };
    let metadata_names = metadata_parameter_names(&built.reference);
    assert_eq!(metadata_names, ["encoder", "value", "decoder"]);
    assert_eq!(
        metadata_parameter_names(&built.krusty),
        metadata_names,
        "generated member parameter names in @Metadata.d2"
    );
    // Instructions from method entry through the last guard. The following body still has
    // independent parity differences and is outside this regression.
    let prologue = |text: &str, member: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains(member))
            .unwrap_or_else(|| panic!("{member} must be present:\n{text}"));
        let instructions = body
            .into_iter()
            .skip(start)
            .skip_while(|line| !line.starts_with("0: "))
            // Stop at this member: a later erased bridge guards its parameters too.
            .take_while(|line| {
                line.split_once(':')
                    .is_some_and(|(pc, _)| !pc.is_empty() && pc.bytes().all(|b| b.is_ascii_digit()))
            })
            .collect::<Vec<_>>();
        let last_guard = instructions
            .iter()
            .rposition(|line| line.contains("checkNotNullParameter"));
        match last_guard {
            Some(at) => instructions[..=at].to_vec(),
            None => Vec::new(),
        }
    };
    for member in ["void serialize(", "deserialize("] {
        let want = prologue(&built.reference, member);
        assert!(
            want.iter()
                .any(|line| line.contains("checkNotNullParameter")),
            "the reference must guard {member} — the rule under test: {want:?}"
        );
        assert_eq!(
            prologue(&built.krusty, member),
            want,
            "{member} entry guards"
        );
    }
}

/// kotlinc splits serialization in two: `$serializer.serialize` opens the structure and DELEGATES
/// the element writes to the serialized class's own `write$Self` static, which is where they live.
///
/// krusty inlined the element writes into `serialize` and left `write$Self` an EMPTY body. That is
/// not only a byte difference: the class exported a do-nothing helper, and generated code in any
/// other module calls it — a `@Serializable` type compiled by krusty and serialized through a
/// sibling module's generated serializer would have written no fields at all.
///
/// Living on the class also changes how a property is read: `write$Self` is a static MEMBER, so it
/// reads the private backing field directly, where the old inlined shape on the `$serializer` had
/// to go through the public getter.
///
/// A GENERIC class keeps the inlined shape for now: kotlinc passes its element serializers to
/// `write$Self` as extra parameters, and krusty's helper has the three-parameter form only — a
/// generic property's encode call reads `this.typeSerial<k>` off the `$serializer` INSTANCE, which a
/// static helper has no receiver for. Emitting the delegation there produced a `getfield` on the
/// wrong owner, which the verifier rejected; the three krusty-only generic serializer tests caught
/// it.
#[test]
fn serialize_delegates_its_element_writes_to_write_self() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let instructions = |text: &str, member: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains(member))
            .unwrap_or_else(|| panic!("{member} must be present:\n{text}"));
        body.into_iter()
            .skip(start)
            .skip_while(|line| !line.starts_with("0: "))
            .take_while(|line| {
                line.split_once(':')
                    .is_some_and(|(pc, _)| !pc.is_empty() && pc.bytes().all(|b| b.is_ascii_digit()))
            })
            .collect::<Vec<_>>()
    };
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializeDelegates",
        SRC,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = instructions(&built.reference, "void serialize(");
    assert!(
        want.iter().any(|line| line.contains("write$Self")),
        "the reference must delegate to write$Self — the rule under test: {want:?}"
    );
    assert_eq!(
        instructions(&built.krusty, "void serialize("),
        want,
        "serialize body"
    );

    // …and the helper it delegates to must actually write the elements.
    let Some(helper) =
        compare_with_kotlinc_plugin("SerializeDelegatesHelper", SRC, "Point", &cp, "25", &extra)
    else {
        return;
    };
    let want = instructions(&helper.reference, "write$Self");
    assert!(
        want.iter().any(|line| line.contains("Element")),
        "the reference's write$Self must encode the elements: {want:?}"
    );
    assert_eq!(
        instructions(&helper.krusty, "write$Self"),
        want,
        "write$Self body"
    );
}

/// Every generated serializer member carries kotlinc's exact debug-table projection. The bare
/// descriptor getter deliberately has locals only; the four executable plugin bodies also map to
/// the annotated declaration's start line.
#[test]
fn generated_serializer_members_carry_their_complete_debug_tables() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializerDebugTables",
        SRC,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };

    fn projection(text: &str, member: &str) -> (Vec<String>, Vec<String>) {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains(member))
            .unwrap_or_else(|| panic!("{member} must be present:\n{text}"));
        let section = body
            .iter()
            .skip(start + 1)
            .take_while(|line| !line.starts_with("public ") && !line.starts_with("private "));
        let mut lines = Vec::new();
        let mut locals = Vec::new();
        let mut table = None;
        for row in section {
            match row.as_str() {
                "LineNumberTable:" => table = Some(false),
                "LocalVariableTable:" => table = Some(true),
                _ if row.ends_with(':') => table = None,
                _ => match table {
                    Some(false) if row.starts_with("line ") => lines.push(
                        row.split_once(':')
                            .map_or_else(|| row.clone(), |(line, _)| line.to_string()),
                    ),
                    Some(true)
                        if row.split_whitespace().next().is_some_and(|value| {
                            value.bytes().all(|byte| byte.is_ascii_digit())
                        }) =>
                    {
                        locals.push(row.split_whitespace().skip(2).collect::<Vec<_>>().join(" "));
                    }
                    _ => {}
                },
            }
        }
        (lines, locals)
    }

    for member in [
        "void serialize(",
        "Point deserialize(",
        "getDescriptor(",
        "childSerializers(",
        "typeParametersSerializers(",
    ] {
        assert_eq!(
            projection(&built.krusty, member),
            projection(&built.reference, member),
            "{member}: complete LineNumberTable + LocalVariableTable projection"
        );
    }
}

/// `serialize` opens on the annotated declaration and maps its trailing return back to the class
/// header. Comparing the complete line table keeps the second entry on the return rather than the
/// preceding `endStructure` call.
#[test]
fn serialize_maps_its_return_to_the_class_header_line() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let lines = |text: &str| {
        let body = structure(text);
        let start = body
            .iter()
            .position(|line| line.contains("void serialize("))
            .expect("serialize must be present");
        body.into_iter()
            .skip(start)
            .skip_while(|line| !line.starts_with("LineNumberTable"))
            .skip(1)
            .take_while(|line| line.starts_with("line "))
            .collect::<Vec<_>>()
    };
    let generic = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Box<T>(val value: T)\n";
    for (name, source, class, exact_offsets) in [
        ("SerializeReturnLine", SRC, "Point$$serializer", true),
        (
            "GenericSerializeReturnLine",
            generic,
            "Box$$serializer",
            false,
        ),
    ] {
        let Some(built) = compare_with_kotlinc_plugin(name, source, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let got = lines(&built.krusty);
        let want = lines(&built.reference);
        if exact_offsets {
            assert_eq!(got, want, "{class}: complete serialize LineNumberTable");
        } else {
            let source_lines = |rows: Vec<String>| {
                rows.into_iter()
                    .map(|row| {
                        row.split_once(':')
                            .map_or(row.clone(), |(line, _)| line.into())
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                source_lines(got),
                source_lines(want),
                "{class}: serialize source-line sequence"
            );
        }
    }
}

/// Instruction rows for one method, with only constant-pool indices erased. javap comments retain
/// the exact selected owner/member/descriptor identity.
pub(super) fn method_instructions(disassembly: &str, marker: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in disassembly.lines() {
        let line = raw.trim();
        if line.ends_with(';') && line.contains(marker) {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if [
            "LineNumberTable",
            "LocalVariableTable",
            "StackMapTable",
            "Exception table",
        ]
        .iter()
        .any(|table| line.starts_with(table))
            || (line.starts_with("descriptor:") && !out.is_empty())
        {
            break;
        }
        let Some((pc, rest)) = line.split_once(": ") else {
            continue;
        };
        if pc.parse::<u32>().is_err() {
            continue;
        }
        let code = rest
            .split_whitespace()
            .map(|token| if token.starts_with('#') { "#" } else { token })
            .collect::<Vec<_>>()
            .join(" ");
        out.push(format!("{}: {code}", pc.trim()));
    }
    out
}

fn instruction_text(row: &str) -> &str {
    row.split_once(": ")
        .map_or(row, |(_, instruction)| instruction)
}

/// Ordered, duplicate-free `(mask word, bit instruction)` projection from deserialize. kotlinc
/// writes each bit in both its sequential and indexed branches; krusty currently has only the
/// indexed branch, so the ABI fact is the stable unique sequence rather than branch duplication.
fn deserialize_mask_updates(instructions: &[String]) -> Vec<(usize, String)> {
    let local = |instruction: &str, opcode: &str| -> Option<u32> {
        let instruction = instruction.strip_prefix(opcode)?;
        let slot = instruction
            .strip_prefix('_')
            .unwrap_or(instruction.trim_start())
            .split_whitespace()
            .next()?;
        slot.parse().ok()
    };
    let mut mask_slots = Vec::new();
    let mut updates = Vec::new();
    for (index, row) in instructions.iter().enumerate() {
        if instruction_text(row) != "ior" || index < 2 || index + 1 >= instructions.len() {
            continue;
        }
        let load = instruction_text(&instructions[index - 2]);
        let bit = instruction_text(&instructions[index - 1]);
        let store = instruction_text(&instructions[index + 1]);
        let loaded = local(load, "iload").expect("seen-mask update loads an int local");
        let stored = local(store, "istore").expect("seen-mask update stores an int local");
        assert_eq!(loaded, stored, "seen-mask update writes its loaded word");
        let word = if let Some(word) = mask_slots.iter().position(|slot| *slot == loaded) {
            word
        } else {
            mask_slots.push(loaded);
            mask_slots.len() - 1
        };
        let update = (word, bit.to_string());
        if !updates.contains(&update) {
            updates.push(update);
        }
    }
    updates
}

fn deserialization_constructor_target(instructions: &[String]) -> String {
    instructions
        .iter()
        .map(|row| instruction_text(row))
        .find(|instruction| {
            instruction.starts_with("invokespecial")
                && instruction.contains("SerializationConstructorMarker")
        })
        .expect("deserialize calls its deserialization constructor")
        .to_string()
}

fn many_fields_source(class: &str, count: usize) -> String {
    let fields = (0..count)
        .map(|index| format!("val f{index:02}: Int"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "import kotlinx.serialization.Serializable\n@Serializable\ndata class {class}({fields})\n"
    )
}

/// Required-mask validation is a complete constructor ABI: generic serializers use their owning
/// class's cached descriptor, and exact 32-field boundaries carry kotlinc's extra zero mask and
/// array-form missing-field report. Every shape is compared as a complete instruction sequence.
#[test]
fn deserialization_constructor_checks_every_mask_shape_exactly() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let generic = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Generic<T>(val required: T, val optional: Int = 7)\n"
        .to_string();
    let value_class = "import kotlinx.serialization.Serializable\n\
                       @JvmInline @Serializable value class Count(val value: Int)\n\
                       @Serializable data class Holder(val count: Count)\n"
        .to_string();
    let cases = [
        ("DeserMaskSmall", SRC.to_string(), "Point".to_string()),
        ("DeserMaskGeneric", generic, "Generic".to_string()),
        (
            "DeserMask32",
            many_fields_source("Fields32", 32),
            "Fields32".to_string(),
        ),
        (
            "DeserMask33",
            many_fields_source("Fields33", 33),
            "Fields33".to_string(),
        ),
        ("DeserMaskValueClass", value_class, "Holder".to_string()),
    ];
    for (tag, source, class) in cases {
        let Some(built) = compare_with_kotlinc_plugin(tag, &source, &class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let marker = "SerializationConstructorMarker)";
        let want = method_instructions(&built.reference, marker);
        assert_eq!(
            method_instructions(&built.krusty, marker),
            want,
            "{class}: complete deserialization-constructor instructions"
        );
        if class == "Generic" {
            let cached_descriptor_rows = |text: &str| {
                structure(text)
                    .into_iter()
                    .filter(|row| row.contains("$cachedDescriptor"))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                cached_descriptor_rows(&built.krusty),
                cached_descriptor_rows(&built.reference),
                "Generic: complete $cachedDescriptor projection"
            );
        }
    }
}

/// A non-constant constructor default is evaluated only when its element bit is absent; retaining
/// just `IrField::default` constants would silently store the decoder local's zero value.
#[test]
fn deserialization_constructor_uses_the_lowered_default_expression() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let source = "import kotlinx.serialization.Serializable\n\
                  fun defaultText(): String = \"hi\"\n\
                  @Serializable\n\
                  data class Defaults(val required: Int, val text: String = defaultText())\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "DeserNonConstantDefault",
        source,
        "Defaults",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        method_instructions(&built.krusty, "SerializationConstructorMarker)"),
        method_instructions(&built.reference, "SerializationConstructorMarker)"),
        "complete non-constant-default constructor instructions"
    );
}

/// `deserialize` owns one seen-mask local per constructor mask word and calls the exact constructor
/// identity recorded by its producer. Complete instructions pin ordinary, generic, exact-word,
/// multi-word, and value-class-marker shapes; substring checks could accept the old primary-ctor
/// fallback or an unrelated synthetic constructor.
#[test]
fn deserialize_passes_every_seen_mask_to_the_exact_constructor() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let generic = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Generic<T>(val required: T, val optional: Int = 7)\n"
        .to_string();
    let value_class = "import kotlinx.serialization.Serializable\n\
                       @JvmInline @Serializable value class Count(val value: Int)\n\
                       @Serializable data class Holder(val count: Count)\n"
        .to_string();
    let cases = [
        ("DeserializeSmall", SRC.to_string(), "Point".to_string()),
        ("DeserializeGeneric", generic, "Generic".to_string()),
        (
            "Deserialize32",
            many_fields_source("Fields32", 32),
            "Fields32".to_string(),
        ),
        (
            "Deserialize33",
            many_fields_source("Fields33", 33),
            "Fields33".to_string(),
        ),
        ("DeserializeValueClass", value_class, "Holder".to_string()),
    ];
    for (tag, source, class) in cases {
        let serializer = format!("{class}$$serializer");
        let Some(built) = compare_with_kotlinc_plugin(tag, &source, &serializer, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        let marker = " deserialize(kotlinx.serialization.encoding.Decoder);";
        let got = method_instructions(&built.krusty, marker);
        let want = method_instructions(&built.reference, marker);
        assert_eq!(
            deserialize_mask_updates(&got),
            deserialize_mask_updates(&want),
            "{class}: ordered mask-word/bit updates"
        );
        assert_eq!(
            deserialization_constructor_target(&got),
            deserialization_constructor_target(&want),
            "{class}: exact constructor owner and descriptor"
        );
    }
}

/// kotlinc emits a `@Serializable` class's generated deserialization constructor after the
/// declared and generated methods, immediately before `<clinit>`.
#[test]
fn a_serializable_class_emits_its_deserialization_constructor_last() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Point(val x: Int, val y: String)\n";
    let Some(built) =
        compare_with_kotlinc_plugin("SerializableCtorOrder", src, "Point", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let members = |text: &str| {
        text.lines()
            .map(str::trim)
            .filter(|line| {
                line.ends_with(';')
                    && (line.starts_with("static {}")
                        || ["public ", "private ", "protected "]
                            .iter()
                            .any(|lead| line.starts_with(lead)))
            })
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        members(&built.krusty),
        members(&built.reference),
        "member order"
    );
}

/// A `@Serializable` class whose ELEMENT type is a `@Serializable` class from the CLASSPATH — another
/// module's jar, which is how every generated API client is shaped. kotlinc reads that dependency's
/// generated serializer straight off the classpath (`getstatic dep/Inner$$serializer.INSTANCE`).
///
/// krusty derived an element serializer only from a `$serializer` declared in the SAME file, so the
/// field had none: first it emitted a `null` child serializer (an NPE at decode), and once that was
/// made explicit it declined the whole file — taking every other class in the module with it.
#[test]
fn an_element_typed_by_a_classpath_serializable_class_uses_its_serializer() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: no scratch dir");
        return;
    };
    // The dependency: built by the reference compiler running its own plugin, exactly as a real jar is.
    let dependency_dir = dir.join("dep");
    std::fs::create_dir_all(&dependency_dir).expect("dependency output directory");
    let dependency_source = dir.join("Dep.kt");
    std::fs::write(
        &dependency_source,
        "package dep\n\
         import kotlinx.serialization.Serializable\n\
         @Serializable\n\
         data class Inner(val a: Int, val b: String)\n",
    )
    .expect("write dependency");
    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        dependency_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-classpath".to_string(),
        cp.iter()
            .map(|jar| jar.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":"),
        format!("-Xplugin={}", plugin.display()),
        dependency_source.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc(dependency) failed: {stderr}");

    let mut classpath = cp.clone();
    classpath.push(dependency_dir.clone());
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               import dep.Inner\n\
               @Serializable\n\
               data class Outer(val inner: Inner)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "ClasspathElementSerializer",
        src,
        "Outer$$serializer",
        &classpath,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let _ = std::fs::remove_dir_all(&dir);
    let want = method_instructions(&built.reference, "childSerializers()");
    assert_ne!(
        want,
        Vec::<String>::new(),
        "kotlinc childSerializers contract"
    );
    assert_eq!(
        method_instructions(&built.krusty, "childSerializers()"),
        want,
        "classpath element serializer instructions"
    );
}

/// The same shape ACROSS FILES of one module: `Outer` in one file, the `@Serializable` `Inner` it
/// stores in another. krusty compiles a file at a time, so `Inner`'s `$serializer` is neither in this
/// file's IR nor yet on the classpath — it is generated as that other file compiles. The element
/// serializer is still `Inner$$serializer.INSTANCE`, and every generated API client is shaped this
/// way: one declaration per file, each storing its siblings.
#[test]
fn an_element_declared_in_another_file_of_the_module_uses_its_serializer() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let sources = [
        (
            "Inner.kt",
            "package model\n\
             import kotlinx.serialization.Serializable\n\
             @Serializable\n\
             data class Inner(val a: Int, val b: String)\n",
        ),
        (
            "Outer.kt",
            "package model\n\
             import kotlinx.serialization.Serializable\n\
             @Serializable\n\
             data class Outer(val inner: Inner)\n",
        ),
    ];
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) =
        compare_files_with_kotlinc_plugin(&sources, "model/Outer$$serializer", &cp, &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_instructions(&built.reference, "childSerializers()");
    assert_ne!(
        want,
        Vec::<String>::new(),
        "kotlinc childSerializers contract"
    );
    assert_eq!(
        method_instructions(&built.krusty, "childSerializers()"),
        want,
        "sibling element serializer instructions"
    );
}

/// A `@Serializable` ENUM stored as an element. An enum has no `$serializer` class of its own — its
/// accessor builds the serializer at run time — so the element reads it through the enum's own
/// `serializer()`, which is what kotlinc caches in `$childSerializers`. krusty derived nothing for it
/// and declined the file; generated API clients are full of enum-typed fields.
#[test]
fn an_element_typed_by_a_serializable_enum_uses_its_accessor() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.SerialName\n\
               @Serializable\n\
               data class Action(val type: Type, val note: String) {\n\
               \x20   @Serializable\n\
               \x20   enum class Type(val value: String) {\n\
               \x20       @SerialName(\"attach\") ATTACH(\"attach\"),\n\
               \x20       @SerialName(\"detach\") DETACH(\"detach\"),\n\
               \x20   }\n\
               }\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializableEnumElement",
        src,
        "Action$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // kotlinc's contract, spelled out so a change in the reference is visible here rather than
    // silently agreed with: the enum's serializer is ALLOCATED, so it is cached and the element is
    // taken out of the cache; the `String` element's singleton is not.
    let want = method_instructions(&built.reference, "childSerializers()");
    assert_eq!(
        want,
        vec![
            "0: invokestatic # // Method Action.access$get$childSerializers$cp:()[Lkotlin/Lazy;",
            "3: astore_1",
            "4: iconst_2",
            "5: anewarray # // class kotlinx/serialization/KSerializer",
            "8: astore_2",
            "9: aload_2",
            "10: iconst_0",
            "11: aload_1",
            "12: iconst_0",
            "13: aaload",
            "14: invokeinterface # 1 // InterfaceMethod kotlin/Lazy.getValue:()Ljava/lang/Object;",
            "19: aastore",
            "20: aload_2",
            "21: iconst_1",
            "22: getstatic # // Field kotlinx/serialization/internal/StringSerializer.INSTANCE:Lkotlinx/serialization/internal/StringSerializer;",
            "25: aastore",
            "26: aload_2",
            "27: areturn",
        ],
        "kotlinc enum element serializer contract"
    );
    assert_eq!(
        method_instructions(&built.krusty, "childSerializers()"),
        want,
        "krusty reads the enum element out of the same cache"
    );
}

/// A contextual element INSIDE a collection. `@file:UseContextualSerialization(T::class)` makes every
/// `T` in the file serialize through a `ContextualSerializer`, and krusty applied that rule to a
/// PROPERTY of type `T` only — a `List<T>` names no such property, so its element serializer was
/// underivable and the file was declined. Generated clients use exactly this to carry loosely-typed
/// JSON maps.
#[test]
fn a_contextual_element_inside_a_collection_is_derivable() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "@file:UseContextualSerialization(Flexible.FlexibleMap::class)\n\
               import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.UseContextualSerialization\n\
               object Flexible {\n\
               \x20   class FlexibleMap\n\
               }\n\
               @Serializable\n\
               data class Options(val tiers: List<Flexible.FlexibleMap>? = null)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "ContextualCollectionElement",
        src,
        "Options$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_instructions(&built.reference, "childSerializers()");
    assert_eq!(
        want,
        vec![
            "0: invokestatic # // Method Options.access$get$childSerializers$cp:()[Lkotlin/Lazy;",
            "3: astore_1",
            "4: iconst_1",
            "5: anewarray # // class kotlinx/serialization/KSerializer",
            "8: astore_2",
            "9: aload_2",
            "10: iconst_0",
            "11: aload_1",
            "12: iconst_0",
            "13: aaload",
            "14: invokeinterface # 1 // InterfaceMethod kotlin/Lazy.getValue:()Ljava/lang/Object;",
            "19: checkcast # // class kotlinx/serialization/KSerializer",
            "22: invokestatic # // Method kotlinx/serialization/builtins/BuiltinSerializersKt.getNullable:(Lkotlinx/serialization/KSerializer;)Lkotlinx/serialization/KSerializer;",
            "25: aastore",
            "26: aload_2",
            "27: areturn",
        ],
        "kotlinc contextual collection element contract"
    );
    assert_eq!(
        method_instructions(&built.krusty, "childSerializers()"),
        want,
        "krusty reads the contextual collection element out of the same cache, and wraps the \
         property's own nullability at the use site exactly where kotlinc does"
    );
}

/// A library type that NAMES its own serializer. `@Serializable(with = JsonObjectSerializer::class)`
/// is how kotlinx's own types are serialized — there is no generated `$serializer` to find, and a
/// consumer storing such a type reads the named class's singleton, which is what kotlinc emits
/// (`getstatic kotlinx/serialization/json/JsonObjectSerializer.INSTANCE`).
///
/// krusty could not see the annotation's ARGUMENT: the classpath recorded annotation identities only,
/// so the element was underivable and the file was declined.
#[test]
fn an_element_whose_class_names_its_own_serializer_reads_that_class() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let Some(json) = gradle_module_jar("org.jetbrains.kotlinx", "kotlinx-serialization-json-jvm")
    else {
        eprintln!("skipping: kotlinx-serialization-json jar not available locally");
        return;
    };
    let mut classpath = cp.clone();
    classpath.push(json);
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               import kotlinx.serialization.json.JsonObject\n\
               @Serializable\n\
               data class Holder(val one: JsonObject)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "NamedSerializerElement",
        src,
        "Holder",
        &classpath,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_instructions(&built.reference, "static {};");
    assert_ne!(
        want,
        Vec::<String>::new(),
        "kotlinc static initializer contract"
    );
    assert_eq!(
        method_instructions(&built.krusty, "static {};"),
        want,
        "named element serializer initialization"
    );
}

/// A NULLABLE element whose type is a `@Serializable` class, an enum, or a collection. `deserialize`
/// required a BUILTIN serializer for any nullable element, so one of these made the whole method
/// undecodable — and krusty then emitted a stub that DEFAULT-CONSTRUCTS the class and ignores the
/// input entirely. `serialize` and `childSerializers` derived the very same element fine.
///
/// kotlinc decodes every nullable element through `decodeNullableSerializableElement` with the same
/// element serializer the other members use.
#[test]
fn a_nullable_serializable_element_is_actually_decoded() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Team(val name: String)\n\
               @Serializable\n\
               enum class Status { ACTIVE, IDLE }\n\
               @Serializable\n\
               data class Account(\n\
               \x20   val team: Team? = null,\n\
               \x20   val status: Status? = null,\n\
               \x20   val tags: List<String>? = null,\n\
               )\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "NullableSerializableElement",
        src,
        "Account$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // The decoder calls `deserialize` makes: a class that decodes nothing calls none of them.
    let decoder_calls = |text: &str| {
        text.lines()
            .skip_while(|line| !line.contains("Account deserialize("))
            .take_while(|line| !line.contains("public java.lang.Object deserialize("))
            .filter_map(|line| line.split("CompositeDecoder.").nth(1))
            .filter_map(|call| call.split(':').next())
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    assert_eq!(
        decoder_calls(&built.reference),
        vec![
            "decodeSequentially",
            "decodeNullableSerializableElement",
            "decodeNullableSerializableElement",
            "decodeNullableSerializableElement",
            "decodeElementIndex",
            "decodeNullableSerializableElement",
            "decodeNullableSerializableElement",
            "decodeNullableSerializableElement",
            "endStructure",
        ],
        "reference decoder contract changed"
    );
    // krusty now makes the SAME calls, in the same order, so the two are compared directly rather
    // than against a second written-down list that could drift from the reference.
    assert_eq!(
        decoder_calls(&built.krusty),
        decoder_calls(&built.reference),
        "krusty must decode all three nullable elements in declaration order, on both paths"
    );
}

/// A file that only uses the serialization plugin still realizes the exact checked serializer
/// accessor selected for a `@Serializable` class declared in a sibling file.
#[test]
fn a_sibling_files_generated_serializer_accessor_matches_kotlinc() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let row = "package demo\n\
        import kotlinx.serialization.Serializable\n\
        @Serializable\n\
        data class Row(val id: String, val count: Int)\n";
    let writer = "package demo\n\
        import kotlinx.serialization.KSerializer\n\
        import kotlinx.serialization.builtins.ListSerializer\n\
        class Writer {\n\
        \x20 fun rows(): KSerializer<List<Row>> = ListSerializer(Row.serializer())\n\
        }\n";
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = compare_files_with_kotlinc_plugin(
        &[("Row.kt", row), ("Writer.kt", writer)],
        "demo/Writer",
        &cp,
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = method_instructions(&built.reference, "rows()");
    assert_eq!(want.len(), 4, "kotlinc rows instruction contract: {want:?}");
    assert_eq!(
        method_instructions(&built.krusty, "rows()"),
        want,
        "sibling serializer accessor instructions"
    );
}

/// Exact `Class.nestedClassName` (protobuf field 7) entries from one class's Kotlin metadata.
///
/// This deliberately reads the field rather than searching `d2`: the string table contains names
/// referenced by every metadata declaration, so presence there does not prove a nested-class edge.
fn metadata_nested_class_names(bytes: &[u8]) -> Option<Vec<String>> {
    fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *bytes.get(*at)?;
            *at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn skip_value(bytes: &[u8], at: &mut usize, wire: u64) -> Option<()> {
        let length = match wire {
            0 => {
                varint(bytes, at)?;
                return Some(());
            }
            1 => 8,
            2 => usize::try_from(varint(bytes, at)?).ok()?,
            5 => 4,
            _ => return None,
        };
        *at = at.checked_add(length)?;
        (*at <= bytes.len()).then_some(())
    }

    let (d1, d2) = common_core::raw_kotlin_metadata(bytes)?;
    // Kotlin's raw-byte encoding begins with NUL, followed by one delimited StringTableTypes
    // message. The remaining bytes are the Class protobuf.
    if d1.first() != Some(&0) {
        return None;
    }
    let mut at = 1;
    let string_table_len = usize::try_from(varint(&d1, &mut at)?).ok()?;
    at = at.checked_add(string_table_len)?;
    if at > d1.len() {
        return None;
    }

    let mut indices = Vec::new();
    while at < d1.len() {
        let tag = varint(&d1, &mut at)?;
        let (field, wire) = (tag >> 3, tag & 7);
        if field != 7 {
            skip_value(&d1, &mut at, wire)?;
            continue;
        }
        if wire != 2 {
            return None;
        }
        let length = usize::try_from(varint(&d1, &mut at)?).ok()?;
        let end = at.checked_add(length)?;
        if end > d1.len() {
            return None;
        }
        while at < end {
            indices.push(usize::try_from(varint(&d1, &mut at)?).ok()?);
        }
    }
    indices
        .into_iter()
        .map(|index| d2.get(index).cloned())
        .collect()
}

/// A `@Serializable` class's generated `$serializer` is a nested classifier of the class, and
/// kotlinc records it under `Class.nestedClassName` beside the `Companion`. krusty listed the
/// companion alone, so a reader of the metadata could not find the serializer as a member of the
/// type it serializes — the plugin generates it, but the language record is the class's own.
#[test]
fn a_serializable_class_lists_its_generated_serializer_as_nested() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Retention(val days: Int)\n";
    let Some(built) =
        compare_with_kotlinc_plugin("NestedSerializer", src, "Retention", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Read Class.nestedClassName (protobuf field 7) itself. Merely searching d2 would be weaker:
    // d2 is the string table for every declaration and can contain the same spelling without any
    // nested-class edge referring to it.
    let want = metadata_nested_class_names(&built.reference_bytes)
        .expect("read kotlinc Class.nestedClassName");
    assert_eq!(
        want,
        ["$serializer", "Companion"],
        "kotlinc records $serializer before Companion"
    );
    let got = metadata_nested_class_names(&built.krusty_bytes)
        .expect("read krusty Class.nestedClassName");
    assert_eq!(
        got, want,
        "nested classifier names of a @Serializable class"
    );
}

/// Whether a javap line opens a MEMBER declaration rather than one of the attribute rows under it.
///
/// `descriptor:` and `Signature:` rows also end in `;` around a parenthesized descriptor, so the
/// `:` is what separates them; constant-pool rows start with `#`. The class initializer prints as
/// `static {};` and has no parameter list at all.
fn opens_a_member(line: &str) -> bool {
    line == "static {};"
        || (line.ends_with(';')
            && line.contains('(')
            && !line.contains(':')
            && !line.starts_with('#'))
}

/// The generic `Signature` of a member of a generated serializer, as javap prints the attribute.
fn member_signature(disassembly: &str, member: &str) -> String {
    // The attribute trails the member's `Code`, so the search runs to the NEXT member declaration
    // rather than a fixed number of lines.
    let declaration = opens_a_member;
    let mut lines = disassembly
        .lines()
        .map(str::trim)
        .skip_while(|line| !(declaration(line) && line.contains(member)));
    lines.next();
    for line in lines {
        // `descriptor:` and `Signature:` both end in `;` and hold a parenthesized descriptor; the
        // `:` is what separates an attribute row from a member declaration.
        if let Some(signature) = line.strip_prefix("Signature: ") {
            return signature
                .split_once("// ")
                .map_or(signature, |(_, text)| text)
                .to_string();
        }
        if declaration(line) {
            break;
        }
    }
    panic!("no Signature attribute for {member}");
}

/// Decode one generated member's Kotlin-metadata return type. The JVM Signature and metadata are
/// separate attributes built by separate emitters, so bytecode parity alone does not cover this.
fn metadata_member_return(bytes: &[u8], owner: &str, member: &str) -> Ty {
    let (d1, d2) = common_core::raw_kotlin_metadata(bytes).expect("read Kotlin metadata");
    let d1 = vec![d1.into_iter().map(char::from).collect::<String>()];
    let metadata = krusty::jvm::metadata::decode_metadata(&d1, &d2, Some(1), owner, None, &[])
        .expect("generated class metadata decodes");
    let matches = metadata
        .class_functions
        .iter()
        .filter(|function| function.kotlin_name == member)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "exactly one metadata member named {member}"
    );
    matches[0]
        .generic_sig
        .as_ref()
        .unwrap_or_else(|| panic!("metadata member {member} must have a return type"))
        .ret
}

/// A generated serializer's array methods hand back serializers whose element types are unrelated
/// to each other — so kotlinc gives them `Array<KSerializer<*>>`, a STAR projection. krusty declared
/// the element type as `KSerializer<Any>`, which is a different Kotlin type: it claims every element
/// serializes `Any`. The distinction exists independently in Kotlin metadata and the JVM `Signature`.
#[test]
fn generated_serializer_arrays_are_star_projected() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Retention(val days: Int)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "StarProjectedSerializers",
        src,
        "Retention$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let expected_type = Ty::obj_args(
        "kotlin/Array",
        &[Ty::obj_args(
            "kotlinx/serialization/KSerializer",
            &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
        )],
    );
    for member in ["childSerializers", "typeParametersSerializers"] {
        let declaration = format!("{member}()");
        let want_signature = member_signature(&built.reference, &declaration);
        assert_eq!(
            want_signature, "()[Lkotlinx/serialization/KSerializer<*>;",
            "kotlinc star-projects {member}'s element serializer type"
        );
        assert_eq!(
            member_signature(&built.krusty, &declaration),
            want_signature,
            "{member} JVM generic signature"
        );
    }

    // kotlinc records the generated override of childSerializers in class metadata. Its
    // typeParametersSerializers realization is synthetic support for the inherited default and is
    // deliberately absent there, though its JVM Signature above still carries the star projection.
    let want_metadata = metadata_member_return(
        &built.reference_bytes,
        "Retention$$serializer",
        "childSerializers",
    );
    assert_eq!(
        want_metadata, expected_type,
        "kotlinc childSerializers metadata type"
    );
    assert_eq!(
        metadata_member_return(
            &built.krusty_bytes,
            "Retention$$serializer",
            "childSerializers",
        ),
        want_metadata,
        "childSerializers Kotlin metadata return type"
    );
}

/// The `d2` string table of a class's Kotlin metadata, read from the class file.
///
/// Every name any declaration in the record refers to is interned here, in the order kotlinc
/// interns them — so comparing the WHOLE table between two compilers pins both which declarations
/// are described and the order they are described in. (Searching it for one name proves much less,
/// which is why a nested-class edge is read from its own protobuf field instead.)
fn metadata_d2(bytes: &[u8]) -> Vec<String> {
    common_core::raw_kotlin_metadata(bytes)
        .expect("read the class's Kotlin metadata")
        .1
}

/// Kotlin-level constructor declarations decoded from one emitted class. This deliberately ignores
/// classfile method flags: metadata publication and visibility are a separate semantic contract.
#[derive(Debug, PartialEq)]
struct ConstructorMetadataShape {
    visibility: krusty::types::Visibility,
    names: Vec<String>,
    defaults: Vec<bool>,
    types: Vec<Ty>,
    jvm_name: &'static str,
    jvm_descriptor: Option<&'static str>,
}

fn metadata_constructors(bytes: &[u8], owner: &str) -> Vec<ConstructorMetadataShape> {
    let (d1, d2) = common_core::raw_kotlin_metadata(bytes).expect("read Kotlin metadata");
    let d1 = vec![d1.into_iter().map(char::from).collect::<String>()];
    krusty::jvm::metadata::decode_metadata(&d1, &d2, Some(1), owner, None, &[])
        .expect("generated class metadata decodes")
        .constructors
        .iter()
        .map(|constructor| ConstructorMetadataShape {
            visibility: constructor.params.visibility,
            names: constructor.params.names.clone(),
            defaults: constructor.params.defaults.clone(),
            types: constructor.params.types.clone(),
            jvm_name: constructor.jvm_name,
            jvm_descriptor: constructor.jvm_desc,
        })
        .collect()
}

#[test]
fn a_serializable_class_publishes_the_exact_deserialization_constructor() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "DeserializationConstructorMetadata",
        SRC,
        "Point",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap are required for byte-parity tests");

    let expected = vec![
        ConstructorMetadataShape {
            visibility: krusty::types::Visibility::Public,
            names: vec!["x".to_string(), "y".to_string()],
            defaults: vec![false, false],
            types: vec![Ty::Int, Ty::String],
            jvm_name: "<init>",
            jvm_descriptor: Some("(ILjava/lang/String;)V"),
        },
        ConstructorMetadataShape {
            visibility: krusty::types::Visibility::Internal,
            names: vec![
                "seen0".to_string(),
                "x".to_string(),
                "y".to_string(),
                "serializationConstructorMarker".to_string(),
            ],
            defaults: vec![false, false, false, false],
            types: vec![
                Ty::Int,
                Ty::Int,
                Ty::nullable(Ty::String),
                Ty::nullable(Ty::obj(
                    "kotlinx/serialization/internal/SerializationConstructorMarker",
                )),
            ],
            jvm_name: "<init>",
            jvm_descriptor: Some(
                "(IILjava/lang/String;Lkotlinx/serialization/internal/SerializationConstructorMarker;)V",
            ),
        },
    ];
    assert_eq!(
        metadata_constructors(&built.reference_bytes, "Point"),
        expected,
        "kotlinc constructor metadata"
    );
    assert_eq!(
        metadata_constructors(&built.krusty_bytes, "Point"),
        expected,
        "krusty constructor metadata"
    );
    if built.krusty_bytes != built.reference_bytes {
        let (want, got) = (structure(&built.reference), structure(&built.krusty));
        let first = want
            .iter()
            .zip(&got)
            .position(|(expected, actual)| expected != actual)
            .unwrap_or_else(|| want.len().min(got.len()));
        panic!(
            "serialized Point differs from kotlinc ({} B vs {} B); first structural difference at line {first}\n  kotlinc: {:?}\n  krusty:  {:?}",
            built.krusty_bytes.len(),
            built.reference_bytes.len(),
            want.get(first),
            got.get(first),
        );
    }
}

/// Everything `@Metadata` says about a generated serializer, in kotlinc's order.
///
/// The record is what a Kotlin consumer reads the declaration back from, and three facts diverged:
/// kotlinc interns the generated functions as `childSerializers`, `deserialize`, `serialize` while
/// krusty used its own emission order; kotlinc describes `descriptor` as a PROPERTY (with its
/// `getDescriptor` accessor) where krusty described neither; and krusty described
/// `typeParametersSerializers`, which kotlinc records only for a GENERIC serializer.
#[test]
fn a_generated_serializer_describes_the_members_kotlinc_describes() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("serialization plugin and runtime must be available under the test harness");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Retention(val days: Int)\n";
    let built = compare_with_kotlinc_plugin(
        "SerializerMemberRecords",
        src,
        "Retention$$serializer",
        &cp,
        "25",
        &extra,
    )
    .expect("reference kotlinc and javap must be available under the test harness");
    let want = metadata_d2(&built.reference_bytes);
    assert!(
        !want
            .iter()
            .any(|entry| entry == "typeParametersSerializers"),
        "a non-generic serializer records no typeParametersSerializers: {want:?}"
    );
    assert_eq!(
        metadata_d2(&built.krusty_bytes),
        want,
        "generated serializer d2"
    );
}

/// The same members for a GENERIC serializer, where kotlinc DOES describe
/// `typeParametersSerializers` — so the member is not simply dropped, it is described exactly when
/// the serializer has type parameters to pass along.
///
/// Only the member list is compared here, not the whole `d2`: a generic serializer's CONSTRUCTOR
/// record also diverges (kotlinc describes the single `(KSerializer)V` form where krusty describes
/// an empty primary plus a `typeSerial0` property), which is a separate change.
#[test]
fn a_generic_serializer_describes_its_type_parameter_serializers() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Boxed<T>(val first: T, val second: String)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "GenericSerializerMemberRecords",
        src,
        "Boxed$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let members = |entries: Vec<String>| {
        entries
            .into_iter()
            .filter(|entry| {
                [
                    "childSerializers",
                    "deserialize",
                    "serialize",
                    "typeParametersSerializers",
                    "descriptor",
                    "getDescriptor",
                ]
                .contains(&entry.as_str())
            })
            .collect::<Vec<_>>()
    };
    let want = members(metadata_d2(&built.reference_bytes));
    assert!(
        want.contains(&"typeParametersSerializers".to_string()),
        "a generic serializer records typeParametersSerializers: {want:?}"
    );
    assert_eq!(
        members(metadata_d2(&built.krusty_bytes)),
        want,
        "generic generated serializer members"
    );
}

/// The `JvmMethodSignature` rule the generated `childSerializers()` depends on, pinned on ORDINARY
/// source declarations so it cannot be mistaken for something specific to the serialization plugin.
///
/// An array's JVM descriptor is built from its element's erasure, and a star projection records no
/// bound to erase — so kotlinc writes the descriptor out for `Array<List<*>>` and leaves it derived
/// for `Array<List<String>>` and for a bare `List<*>`, whose erasure is its own classifier.
#[test]
fn only_an_array_of_a_star_projection_records_its_descriptor() {
    let src = "class Probe {\n\
               \x20 fun stars(xs: Array<List<*>>): Int = xs.size\n\
               \x20 fun plain(xs: Array<List<String>>): Int = xs.size\n\
               \x20 fun one(xs: List<*>): Int = xs.size\n\
               }\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "StarArraySignature",
        src,
        "Probe",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = metadata_d2(&built.reference_bytes);
    assert_eq!(
        want.iter().filter(|entry| entry.contains('(')).count(),
        2,
        "the constructor and exactly one member record a descriptor: {want:?}"
    );
    assert!(
        want.contains(&"([Ljava/util/List;)I".to_string()),
        "the star-projected array is the one recorded: {want:?}"
    );
    assert_eq!(metadata_d2(&built.krusty_bytes), want, "Probe d2");
}

/// One member's disassembly, from its declaration to the next one, with pool indices erased.
pub(super) fn member_body(disassembly: &str, member: &str) -> Vec<String> {
    let declaration = opens_a_member;
    // Raw lines, because INDENTATION is what separates the class body's closing `}` (column 0)
    // from a `}` inside a member — a `tableswitch` block ends with one, and trimming first made
    // this stop there and silently return a truncated body.
    let mut lines = disassembly
        .lines()
        .skip_while(|line| !(declaration(line.trim()) && line.contains(member)));
    let mut body = vec![lines.next().unwrap_or_default().trim().to_string()];
    for line in lines {
        // The LAST member ends at the class body's close, not at another declaration.
        if line == "}" || declaration(line.trim()) {
            break;
        }
        body.push(line.trim().to_string());
    }
    structure(&body.join("\n"))
}

/// A generated serializer's `<init>` carries kotlinc's debug tables: a line table rooted at the
/// annotated owner's declaration and a local-variable table naming `this`. Every other generated
/// member already did; the constructor is emitted from the class declaration rather than from a
/// generated `IrFunction`, and the synthesized class had no declaration line to root them at.
#[test]
fn a_generated_serializer_constructor_carries_its_debug_tables() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               \n\
               @Serializable\n\
               data class Retention(val days: Int)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializerInitDebug",
        src,
        "Retention$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = member_body(&built.reference, "Retention$$serializer()");
    assert!(
        want.iter().any(|line| line == "line 3: 0")
            && want.iter().any(|line| line.contains("this")),
        "kotlinc roots the constructor at the annotated declaration: {want:?}"
    );
    assert_eq!(
        member_body(&built.krusty, "Retention$$serializer()"),
        want,
        "generated serializer constructor"
    );
}

/// Declaring one accessor does not make the other accessor declared. In particular, a `var` with
/// an explicit getter still has a synthesized default setter, whose own line/local tables must not
/// be dropped while the generated serializer's declared getter is excluded from synthesis.
#[test]
fn an_explicit_getter_keeps_its_default_setter_debug_tables() {
    let src = "class Holder {\n\
               \x20 var value: String = \"initial\"\n\
               \x20     get() = field\n\
               }\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "MixedPropertyAccessors",
        src,
        "Holder",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        member_body(&built.krusty, "setValue(java.lang.String)"),
        member_body(&built.reference, "setValue(java.lang.String)"),
        "default setter beside an explicit getter"
    );
}

/// Where the `Deprecated` attribute name lands in the constant pool of a class that carries the
/// attribute itself.
///
/// It was interned with the per-method attribute names, ahead of `InnerClasses` and `SourceFile`;
/// kotlinc interns a class-level one after both, immediately before `RuntimeVisibleAnnotations`.
/// Nothing else about the class changes — a pool in a different order is simply a different class
/// file, and this was the last byte between the two on a generated serializer.
///
/// The generated `$serializer` is the fixture because it is the only class krusty marks deprecated
/// today: a source `@Deprecated` declaration gets no class-level attribute at all, which is a
/// separate gap.
#[test]
fn a_deprecated_class_attribute_name_interns_after_its_source_file() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Point(val x: Int, val y: String)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "DeprecatedPool",
        src,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // The three attribute names in the order the pool holds them. Their absolute indices differ
    // while anything else about the class does; their order is the contract.
    let order = |text: &str| {
        let pool = text
            .lines()
            .map(str::trim)
            .take_while(|line| !line.starts_with('{'))
            .filter_map(|line| line.split("= Utf8").nth(1))
            .map(str::trim)
            .collect::<Vec<_>>();
        ["Deprecated", "InnerClasses", "SourceFile"]
            .into_iter()
            .filter_map(|name| Some((pool.iter().position(|entry| *entry == name)?, name)))
            .collect::<Vec<_>>()
    };
    let mut want = order(&built.reference);
    want.sort();
    assert_eq!(
        want.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["InnerClasses", "SourceFile", "Deprecated"],
        "kotlinc interns a class-level Deprecated after both"
    );
    let mut got = order(&built.krusty);
    got.sort();
    assert_eq!(
        got.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        want.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        "class attribute name interning order"
    );
}

/// The class initializer that builds a singleton serializer's `descriptor`.
///
/// Three divergences, all invisible to a running program:
///
///   * krusty read the serializer through `this`, which made the emitter hoist `INSTANCE` into a
///     local for the class initializer — one store, one load and one extra local kotlinc does not
///     have. kotlinc reads `INSTANCE` at the use site.
///   * neither narrowing cast was emitted: `INSTANCE` to the `GeneratedSerializer` the descriptor's
///     constructor takes, and the built descriptor to the `SerialDescriptor` the field holds.
///   * a generated class has no per-statement source to map, so kotlinc gives its `<clinit>` two
///     line entries — the body at the declaration line, the trailing `return` at the declaration's
///     closing line. krusty emitted no `LineNumberTable` at all.
#[test]
fn a_singleton_serializers_class_initializer_matches_kotlinc() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    // A multi-line declaration, so the closing-brace entry is a DIFFERENT line from the header and
    // the two cannot be confused.
    let src = "import kotlinx.serialization.Serializable\n\
               \n\
               @Serializable\n\
               data class Repo(\n\
               \x20   val id: Long,\n\
               \x20   val name: String,\n\
               )\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "SerializerClassInit",
        src,
        "Repo$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let want = member_body(&built.reference, "static {}");
    assert!(
        want.iter().any(|line| line == "line 3: 10")
            && want.iter().any(|line| line.starts_with("line 7:")),
        "kotlinc maps the body to the declaration and the return to its closing line: {want:?}"
    );
    assert_eq!(
        member_body(&built.krusty, "static {}"),
        want,
        "generated serializer class initializer"
    );
}

/// What `deserialize` opens before it decodes anything, and how it dispatches on the element index.
///
/// The local layout is kotlinc's: the descriptor read ONCE into a local, then the loop flag, the
/// element index, the seen-mask, the field locals, and the composite decoder LAST — `beginStructure`
/// runs after the rest are zeroed. krusty had kept the composite decoder at slot 2 and re-read
/// `this.descriptor` at every use, which put every subsequent local one place off.
///
/// The dispatch is one `tableswitch` over `-1..=n-1` with a default, not a chain of comparisons.
///
/// Only the prologue is compared instruction for instruction; the rest of the method is pinned by
/// the dispatch assertion below and by the runtime differentials in
/// `deserialize_dispatch_shape_e2e`.
#[test]
fn deserialize_opens_kotlincs_locals_and_switches_on_the_index() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class Point(val x: Int, val y: String)\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "DeserializeDispatch",
        src,
        "Point$$serializer",
        &cp,
        "25",
        &extra,
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Which local each value lands in, up to the `beginStructure` result — the layout every later
    // instruction indexes into. Offsets are not compared: they shift by the one documented extra
    // instruction below.
    let stores = |text: &str| {
        let body = member_body(text, "deserialize(kotlinx.serialization.encoding.Decoder)");
        let end = body
            .iter()
            .position(|line| line.contains("beginStructure"))
            .unwrap_or_else(|| panic!("no beginStructure in deserialize: {body:?}"));
        body[..=end]
            .iter()
            .filter_map(|line| line.split_once(": "))
            .map(|(_, instruction)| instruction.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|instruction| {
                instruction.starts_with("astore") || instruction.starts_with("istore")
            })
            .collect::<Vec<_>>()
    };
    let want = stores(&built.reference);
    assert_eq!(
        want,
        ["astore_2", "istore_3", "istore 5", "istore 6", "astore 7"],
        "kotlinc's locals: the descriptor, the loop flag, the seen mask, then the fields"
    );
    // Slot 4 is the element index, which neither compiler initializes — it is assigned only by the
    // loop, and the verifier frames carry `top` there until then.
    assert_eq!(stores(&built.krusty), want, "deserialize local layout");

    let dispatch = |text: &str| {
        member_body(text, "deserialize(kotlinx.serialization.encoding.Decoder)")
            .into_iter()
            .filter(|line| {
                line.contains("tableswitch")
                    || line.contains("lookupswitch")
                    || line.contains("UnknownFieldException")
            })
            .map(|line| {
                line.split_once(": ")
                    .map_or(line.clone(), |(_, rest)| rest.to_string())
            })
            .collect::<Vec<_>>()
    };
    let want_dispatch = dispatch(&built.reference);
    assert!(
        want_dispatch
            .iter()
            .any(|line| line.contains("tableswitch"))
            && want_dispatch
                .iter()
                .any(|line| line.contains("UnknownFieldException")),
        "kotlinc switches on the element index and throws on an unknown one: {want_dispatch:?}"
    );
    assert_eq!(
        dispatch(&built.krusty),
        want_dispatch,
        "element-index dispatch"
    );
}
/// `ACC_SYNTHETIC` does not cross a compilation boundary.
///
/// A generated `$serializer`'s `InnerClasses` row carries `ACC_SYNTHETIC` (0x1019) in the module
/// that DECLARES it, and `0x0019` in every module that only references it: kotlinc knows a class is
/// compiler-generated while it is compiling it, and a class read back from the classpath is just a
/// declaration. krusty copied the bit off the dependency's own row.
///
/// `javap` prints neither form's `ACC_SYNTHETIC` on an `InnerClasses` line, so this reads the raw
/// `access_flags`; an eyeballed expectation would have accepted the wrong value.
#[test]
fn a_classpath_serializers_inner_classes_row_drops_acc_synthetic() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let stem = "ClasspathSerializerSynthetic";
    let Some(work) = common::scratch_dir().map(|dir| dir.join(stem)) else {
        eprintln!("skipping: no scratch directory");
        return;
    };
    let dependency = work.join("dep");
    std::fs::create_dir_all(&dependency).expect("create dependency directory");
    let dep_source = work.join("Dep.kt");
    std::fs::write(
        &dep_source,
        "import kotlinx.serialization.Serializable\n\
         @Serializable\n\
         data class Dep(val id: Int)\n",
    )
    .expect("write dependency source");
    let mut arguments = vec![
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-classpath".to_string(),
        cp.iter()
            .map(|jar| jar.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":"),
        "-d".to_string(),
        dependency.to_string_lossy().into_owned(),
    ];
    arguments.push(dep_source.to_string_lossy().into_owned());
    let Some((code, stderr)) = common::kotlinc_compile(&arguments) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(code, 0, "{stem}: kotlinc rejected the dependency: {stderr}");

    // The dependency's OWN row is the control: it must still carry the bit.
    let dep_bytes = std::fs::read(dependency.join("Dep$$serializer.class"))
        .expect("the dependency emitted its serializer");
    assert_eq!(
        inner_class_access(&dep_bytes, "Dep$$serializer"),
        Some(0x1019),
        "{stem}: the declaring module's own row carries ACC_SYNTHETIC"
    );

    // Both sides of the consumer are compiled here rather than through
    // `compare_with_kotlinc_plugin`, which allocates a scratch directory of its own and hands back
    // disassembly — and `javap` is exactly the tool that cannot show this bit.
    let mut consumer_cp = cp.clone();
    consumer_cp.push(dependency.clone());
    let reference = work.join("ref");
    std::fs::create_dir_all(&reference).expect("create reference directory");
    let src = "import kotlinx.serialization.Serializable\n\
               @Serializable\n\
               data class User(val dep: Dep)\n";
    let user_source = work.join("User.kt");
    std::fs::write(&user_source, src).expect("write consumer source");
    let (code, stderr) = common::kotlinc_compile(&[
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-classpath".to_string(),
        consumer_cp
            .iter()
            .map(|entry| entry.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":"),
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        user_source.to_string_lossy().into_owned(),
    ])
    .expect("the reference compiler was available for the dependency, so it is here too");
    assert_eq!(code, 0, "{stem}: kotlinc rejected the consumer: {stderr}");
    let reference_bytes = std::fs::read(reference.join("User$$serializer.class"))
        .expect("kotlinc emitted the consumer's serializer");

    let classes = common::compile_in_process_metadata_cp_module_target(
        src,
        stem,
        &consumer_cp,
        "main",
        Some(69),
    )
    .unwrap_or_else(|| panic!("{stem}: krusty failed to compile the consumer"));
    let (_, krusty_bytes) = classes
        .iter()
        .find(|(name, _)| name == "User$$serializer")
        .unwrap_or_else(|| panic!("{stem}: krusty emitted no User$$serializer"));

    const CLASSPATH_ROW: u16 = 0x0019;
    assert_eq!(
        inner_class_access(&reference_bytes, "Dep$$serializer"),
        Some(CLASSPATH_ROW),
        "{stem}: kotlinc's row for the CLASSPATH serializer"
    );
    assert_eq!(
        inner_class_access(krusty_bytes, "Dep$$serializer"),
        Some(CLASSPATH_ROW),
        "{stem}: krusty's row for the CLASSPATH serializer"
    );
    // The consumer's OWN generated serializer still carries the bit, on both sides.
    assert_eq!(
        inner_class_access(krusty_bytes, "User$$serializer"),
        inner_class_access(&reference_bytes, "User$$serializer"),
        "{stem}: the consumer's own row"
    );
}

/// The raw `access_flags` of the `InnerClasses` row naming `inner`, or `None` when there is none.
fn inner_class_access(bytes: &[u8], inner: &str) -> Option<u16> {
    krusty::jvm::classreader::parse_class(bytes)
        .ok()?
        .inner_classes
        .iter()
        .find(|entry| entry.inner == inner)
        .map(|entry| entry.access)
}
/// The ordered `Utf8` entries of a `javap -v` constant pool, filtered to the ones asked for.
///
/// The whole pool cannot be compared here: `@Metadata`'s `d1` strings are pool entries too, and the
/// in-process compilation path these helpers take records different function flags there than the
/// shipped CLI. The ORDER of the entries this test names is the fact at issue and is unaffected.
fn pool_utf8_order(disassembly: &str, wanted: &[&str]) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .filter_map(|line| line.split_once("= Utf8"))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| wanted.contains(&value.as_str()))
        .collect()
}

/// Each property's `$annotations` marker is emitted directly after that property's accessors, and
/// kotlinc interns its name and annotation entries THERE — before `componentN`. A private property
/// has no accessor, so its marker occupies the property's position on its own.
///
/// A data class seeds its synthesized members' pool entries in one pass before any member is
/// emitted, and that seeder knew nothing about the marker, so the marker's four entries landed
/// about forty slots late. Every member matched, every attribute matched, and the class still
/// differed from kotlinc's byte for byte: the pool is part of the output.
#[test]
fn property_markers_follow_their_public_or_private_property_before_componentn() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
               @Target(AnnotationTarget.PROPERTY)\n\
               @Retention(AnnotationRetention.RUNTIME)\n\
               annotation class OwnedMark(val label: String)\n\
               @Serializable\n\
               data class Perm(\n\
               \x20   @OwnedMark(\"public_val\") val publicVal: Boolean,\n\
               \x20   @OwnedMark(\"public_var\") var publicVar: Int,\n\
               \x20   @OwnedMark(\"private_val\") private val privateVal: Long,\n\
               \x20   val plain: Int,\n\
               )\n";
    let Some(built) =
        compare_with_kotlinc_plugin("PropertyMarkerPool", src, "Perm", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let wanted = [
        "getPublicVal",
        "getPublicVal$annotations",
        "LOwnedMark;",
        "label",
        "public_val",
        "getPublicVar",
        "setPublicVar",
        "getPublicVar$annotations",
        "public_var",
        "getPrivateVal$annotations",
        "private_val",
        "getPlain",
        "component1",
        "component2",
        "component3",
        "component4",
        "copy",
    ];
    let want = pool_utf8_order(&built.reference, &wanted);
    assert_eq!(
        want,
        vec![
            "getPublicVal",
            "getPublicVal$annotations",
            "LOwnedMark;",
            "label",
            "public_val",
            "getPublicVar",
            "setPublicVar",
            "getPublicVar$annotations",
            "public_var",
            "getPrivateVal$annotations",
            "private_val",
            "getPlain",
            "component1",
            "component2",
            "component3",
            "component4",
            "copy",
        ],
        "kotlinc's interning order, stated so a change in the reference is visible here"
    );
    assert_eq!(
        pool_utf8_order(&built.krusty, &wanted),
        want,
        "krusty interns the marker where kotlinc does"
    );
}
