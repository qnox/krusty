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
               data class Outer(val inner: Inner, val items: List<Inner>)\n";
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
    // The dependency's own generated serializer, referenced by name.
    assert!(
        built.reference.contains("dep/Inner$$serializer.INSTANCE"),
        "reference must read the dependency's serializer — that is the rule under test:\n{}",
        built.reference
    );
    assert!(
        built.krusty.contains("dep/Inner$$serializer.INSTANCE"),
        "krusty must read the dependency's serializer instead of deriving nothing:\n{}",
        built.krusty
    );
    // A `null` child serializer is the shape that used to reach the runtime as an NPE.
    let child_serializers = |text: &str| {
        text.lines()
            .skip_while(|line| !line.contains("childSerializers()"))
            .take_while(|line| !line.contains("typeParametersSerializers"))
            .filter(|line| line.contains("aconst_null"))
            .count()
    };
    assert_eq!(
        child_serializers(&built.krusty),
        child_serializers(&built.reference),
        "krusty must not leave a null child serializer:\n{}",
        built.krusty
    );
}

/// The same shape ACROSS FILES of one module: `Outer` in one file, the `@Serializable` `Inner` it
/// stores in another. krusty compiles a file at a time, so `Inner`'s `$serializer` is neither in this
/// file's IR nor yet on the classpath — it is generated as that other file compiles. The element
/// serializer is still `Inner$$serializer.INSTANCE`, and every generated API client is shaped this
/// way: one declaration per file, each storing its siblings.
#[test]
fn an_element_declared_in_another_file_of_the_module_uses_its_serializer() {
    let Some((_, cp)) = plugin_and_runtime() else {
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
             data class Outer(val inner: Inner, val items: List<Inner>)\n",
        ),
    ];
    let classes = common::compile_in_process_files(&sources, &cp, None)
        .expect("krusty compiles both files of the module");
    let serializer = classes
        .iter()
        .find(|(internal, _)| internal.ends_with("Outer$$serializer"))
        .map(|(_, bytes)| bytes.clone())
        .expect("the outer class's generated serializer is emitted");
    // The dependency's serializer is named in the constant pool as an ordinary class reference; a
    // derived-nothing serializer names it nowhere.
    let pool = String::from_utf8_lossy(&serializer).into_owned();
    assert!(
        pool.contains("model/Inner$$serializer"),
        "krusty must reference the sibling file's generated serializer"
    );
}
