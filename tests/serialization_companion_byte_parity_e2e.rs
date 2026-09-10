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
//! The comparison drops `LineNumberTable`/`LocalVariableTable`: krusty emits neither for a
//! plugin-generated member (both are gated on a source declaration line, which a synthesized
//! function has none of). That is the remaining gap between this class and byte identity, and it is
//! deliberately not asserted here rather than silently normalized away everywhere — see the filter.
use std::path::PathBuf;

use super::common;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Point(val x: Int, val y: String)\n";

/// The serialization runtime the generated code links against, plus the plugin jar kotlinc needs in
/// order to produce the reference at all. `None` when either is absent from the local caches.
fn plugin_and_runtime() -> Option<(PathBuf, Vec<PathBuf>)> {
    let plugin = common::kotlinc_plugin_jar("kotlinx-serialization-compiler-plugin")?;
    let core =
        common::gradle_module_jar("org.jetbrains.kotlinx", "kotlinx-serialization-core-jvm")?;
    Some((plugin, vec![core, common::stdlib_jar()]))
}

/// javap output reduced to what this test asserts: member signatures, flags, instructions and the
/// `InnerClasses` table, with constant-pool indices erased (they are an emission-order artifact) and
/// the debug tables dropped.
fn structure(disassembly: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut skipping = false;
    for raw in disassembly.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        // The constant pool is an emission-order artifact: same entries, different numbering, and
        // the whole point of a structural comparison is not to depend on it.
        if trimmed.starts_with("Constant pool:") || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("LineNumberTable:") || trimmed.starts_with("LocalVariableTable:") {
            skipping = true;
            continue;
        }
        if skipping {
            // A skipped block ends at the next line that is not one of its indented rows.
            let indent = line.len() - line.trim_start().len();
            if trimmed.is_empty() || indent <= 4 {
                skipping = false;
            } else {
                continue;
            }
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

#[test]
fn serializable_companion_matches_kotlinc_structure() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let Some(built) = common::disassemble_against_kotlinc_plugin(
        "SerializableCompanion",
        SRC,
        "Point$Companion",
        &cp,
        Some("25"),
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
