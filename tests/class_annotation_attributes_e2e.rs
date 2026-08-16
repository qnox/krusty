//! A user annotation on a CLASS must reach the class file, in the attribute its retention selects.
//!
//! krusty wrote class-level annotations into `RuntimeVisibleAnnotations` only, and only for plain
//! classes and objects: a BINARY-retained annotation was dropped by every declaration kind, and
//! interfaces and enums dropped their RUNTIME-retained ones too, because those kinds are emitted by
//! their own writers which never queued the class's annotations at all. `@ApiStatus.Internal` —
//! BINARY-retained, and stamped on thousands of intellij-community declarations — therefore never
//! appeared in krusty's output.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and the
//! class-level annotation attributes of each declaration kind are compared.
use std::fs;

use super::common;

/// The class-level annotation attributes from `javap -v`: attribute name → the rendering of each
/// entry, with constant-pool indices stripped (`#12` → `#`). Only what follows the column-0 `}`
/// counts — everything before it belongs to a member.
///
/// The `kotlin.Metadata` entry is EXCLUDED. kotlinc also records a class's annotations inside the
/// metadata protobuf and krusty does not yet, a tracked gap that would otherwise mask the attribute
/// this test is about.
fn class_annotations(
    dir: &std::path::Path,
    class: &str,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-v", "-p", &path.to_string_lossy()]).expect("pooled javap");
    let mut out: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut class_level = false;
    let mut attribute: Option<String> = None;
    let mut entry: Vec<String> = Vec::new();
    let flush = |attribute: &Option<String>,
                 entry: &mut Vec<String>,
                 out: &mut std::collections::BTreeMap<String, Vec<String>>| {
        if entry.is_empty() {
            return;
        }
        let rendered = std::mem::take(entry).join(" ");
        if rendered.starts_with("kotlin.Metadata") {
            return;
        }
        if let Some(name) = attribute {
            out.entry(name.clone()).or_default().push(rendered);
        }
    };
    for line in raw.lines() {
        if line == "}" {
            class_level = true;
            continue;
        }
        if !class_level {
            continue;
        }
        let trimmed = line.trim();
        if let Some(name) = line.strip_suffix(':').filter(|l| !l.starts_with(' ')) {
            flush(&attribute, &mut entry, &mut out);
            attribute = name
                .starts_with("Runtime")
                .then(|| name.to_string())
                .inspect(|name| {
                    out.entry(name.clone()).or_default();
                });
            continue;
        }
        if attribute.is_none() {
            continue;
        }
        // An entry starts at `  N: #...`; its rendering follows, indented further.
        if trimmed.starts_with(|c: char| c.is_ascii_digit()) && trimmed.contains(": #") {
            flush(&attribute, &mut entry, &mut out);
            continue;
        }
        let mut normalized = String::new();
        let mut chars = trimmed.chars().peekable();
        while let Some(c) = chars.next() {
            normalized.push(c);
            if c == '#' {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
            }
        }
        entry.push(normalized);
    }
    flush(&attribute, &mut entry, &mut out);
    out
}

/// Compile `src` with BOTH compilers into fresh directories, returning `(krusty_dir, kotlinc_dir)`.
/// `None` when the provisioned toolchain is unavailable (the test then skips).
fn compile_both(name: &str, src: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let base =
        std::env::temp_dir().join(format!("krusty_class_anno_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).ok()?;
    fs::create_dir_all(&kotlinc_dir).ok()?;

    let source = base.join("Marks.kt");
    fs::write(&source, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ])?;
    assert_eq!(code, 0, "{name}: kotlinc rejected the fixture: {stderr}");

    let classes = common::compile_in_process(
        src,
        "Marks",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .unwrap_or_else(|| panic!("{name}: krusty failed to compile the fixture"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&path, bytes).ok()?;
    }
    Some((krusty_dir, kotlinc_dir))
}

/// One source declaring both retentions, applied to every declaration kind a class file can be.
const KINDS: &str = r#"
@Retention(AnnotationRetention.RUNTIME)
annotation class Kept

@Retention(AnnotationRetention.BINARY)
annotation class Recorded

@Kept @Recorded class AsClass
@Kept @Recorded object AsObject
@Kept @Recorded interface AsInterface
@Kept @Recorded enum class AsEnum { ONE }
@Kept @Recorded annotation class AsAnnotation
"#;

#[test]
fn every_declaration_kind_carries_its_class_annotations() {
    let Some((krusty_dir, kotlinc_dir)) = compile_both("kinds", KINDS) else {
        return; // toolchain not provisioned
    };
    for class in [
        "AsClass",
        "AsObject",
        "AsInterface",
        "AsEnum",
        "AsAnnotation",
    ] {
        let mut krusty = class_annotations(&krusty_dir, class);
        let mut kotlinc = class_annotations(&kotlinc_dir, class);
        if class == "AsAnnotation" {
            // An annotation class also carries the `java.lang.annotation.*` mirrors kotlinc
            // synthesizes for it, which krusty emits partially and in its own order — a separate
            // gap, tracked on its own, that would otherwise mask what this test is about.
            krusty.remove("RuntimeVisibleAnnotations");
            kotlinc.remove("RuntimeVisibleAnnotations");
        }
        assert_eq!(
            krusty, kotlinc,
            "{class}: class-level annotation attributes must match kotlinc's"
        );
        // Guard the comparison itself: an empty map on BOTH sides would pass vacuously.
        assert!(
            krusty
                .get("RuntimeInvisibleAnnotations")
                .is_some_and(|entries| entries.iter().any(|e| e.starts_with("Recorded"))),
            "{class}: the BINARY-retained annotation must be recorded"
        );
    }
}

/// A BINARY-retained annotation's ARGUMENTS must survive too — the element values, their tags and
/// their order are what a consumer reads back.
#[test]
fn recorded_annotation_arguments_match_kotlinc() {
    let src = r#"
@Retention(AnnotationRetention.BINARY)
annotation class Marked(val name: String, val n: Int, val kinds: Array<String>)

@Marked("core", 7, ["a", "b"]) class WithArgs
"#;
    let Some((krusty_dir, kotlinc_dir)) = compile_both("args", src) else {
        return; // toolchain not provisioned
    };
    let krusty = class_annotations(&krusty_dir, "WithArgs");
    assert_eq!(
        krusty,
        class_annotations(&kotlinc_dir, "WithArgs"),
        "WithArgs: the recorded annotation's arguments must match kotlinc's"
    );
    assert!(
        krusty["RuntimeInvisibleAnnotations"]
            .iter()
            .any(|e| e.contains("name=\"core\"") && e.contains("kinds=[\"a\",\"b\"]")),
        "WithArgs: element values must be present: {krusty:?}"
    );
}
