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
/// class's `PluginGeneratedSerialDescriptor(<name>, …)`.
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
               \x20   }\n\
               }\n";
    // The enum carries its own serial name; a class's lives on the generated `$serializer`.
    for (class, expected) in [
        ("Outer$Middle$Phase", "Outer.Middle.Phase"),
        ("Outer$Middle$Point$$serializer", "Outer.Middle.Point"),
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
