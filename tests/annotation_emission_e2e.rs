//! Declared annotations must reach the class file exactly as kotlinc writes them.
//!
//! krusty dropped every method-level user annotation, so an annotated function diverged from the
//! reference bytes and a consumer could not read the declaration back — `@Deprecated(level =
//! HIDDEN)` in particular, whose entire purpose is to be read back by overload resolution.
//!
//! These are DIFFERENTIAL: the same source is compiled by the provisioned kotlinc and by krusty,
//! and each declaration's `javap -v` attribute section is compared. That pins the annotation
//! payload, the `Deprecated` attribute, `ACC_SYNTHETIC` on a hidden declaration, and the attribute
//! ORDER — the facts a shape-only assertion would miss.
use std::fs;

use super::common;

/// Per-DECLARATION annotation facts from `javap -v`, keyed by the declaration line: its flags and
/// its annotation attributes, with constant-pool indices stripped (`#12` → `#`). Class-level
/// attributes (notably `@Metadata`) are excluded — kotlinc also records annotations in the
/// metadata protobuf, which krusty does not write yet; that gap is tracked separately and would
/// otherwise mask what this comparison is for.
fn annotation_shapes(
    dir: &std::path::Path,
    class: &str,
) -> std::collections::HashMap<String, Vec<String>> {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-v", "-p", &path.to_string_lossy()]).expect("pooled javap");
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut current: Option<String> = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        // The class body ends at a column-0 `}`; everything after it is class-level.
        if line == "}" {
            break;
        }
        if !line.starts_with("    ") && trimmed.ends_with(");") && !trimmed.starts_with('#') {
            current = Some(trimmed.to_string());
            out.entry(trimmed.to_string()).or_default();
            continue;
        }
        let Some(declaration) = current.as_ref() else {
            continue;
        };
        let keep = trimmed.starts_with("flags:")
            || trimmed.starts_with("Deprecated:")
            || trimmed.starts_with("RuntimeVisibleAnnotations")
            || trimmed.starts_with("RuntimeInvisibleAnnotations")
            || trimmed.starts_with("RuntimeInvisibleParameterAnnotations")
            || trimmed.starts_with("0: #")
            || trimmed.starts_with("1: #")
            || trimmed.ends_with('(')
            || (trimmed.contains('=')
                && !trimmed.contains("descriptor:")
                && !trimmed.contains(", locals="));
        if !keep {
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
        out.entry(declaration.clone()).or_default().push(normalized);
    }
    out
}

/// The class's `Constant pool:` listing, one entry per line with the index dropped. Excludes the
/// `@Metadata` payload (a `Utf8` whose text starts with a NUL) — kotlinc records annotations in the
/// metadata protobuf and krusty does not yet, a tracked gap that would otherwise mask everything
/// else. Pool ORDER is what the annotation-reservation and attribute-name interning fixes are
/// about, so this compares the sequence, not a set.
fn constant_pool(dir: &std::path::Path, class: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-v", "-p", &path.to_string_lossy()]).expect("pooled javap");
    let mut out = Vec::new();
    let mut in_pool = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == "Constant pool:" {
            in_pool = true;
            continue;
        }
        if !in_pool {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('{') {
            break;
        }
        let Some((_, entry)) = trimmed.split_once('=') else {
            continue;
        };
        let entry = entry.trim();
        if entry.starts_with("Utf8") && entry.contains("\\u0000") {
            continue;
        }
        // Entries reference other entries by index; drop those too, so the comparison is about the
        // sequence of KINDS and literal text.
        let mut normalized = String::new();
        let mut chars = entry.chars().peekable();
        while let Some(c) = chars.next() {
            normalized.push(c);
            if c == '#' {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
            }
        }
        out.push(normalized);
    }
    out
}

/// Compile `src` with BOTH compilers into fresh directories, returning `(krusty_dir, kotlinc_dir)`.
/// `None` when the provisioned toolchain is unavailable (the test then skips).
fn compile_both(
    name: &str,
    file: &str,
    src: &str,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let base = std::env::temp_dir().join(format!("krusty_anno_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).ok()?;
    fs::create_dir_all(&kotlinc_dir).ok()?;

    let source = base.join(file);
    fs::write(&source, src).ok()?;
    let stdlib = common::stdlib_jar();
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ])?;
    assert_eq!(code, 0, "{name}: kotlinc rejected the fixture: {stderr}");

    let stem = file.strip_suffix(".kt").expect("fixture is a .kt file");
    let classes =
        common::compile_in_process(src, stem, &[stdlib], Some(common::jdk_modules().as_path()))
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

/// Compare the annotation facts of every declaration whose javap line contains one of `decls`.
/// Selecting declarations keeps the assertion independent of the order krusty emits methods in,
/// which is a separate parity concern.
fn assert_same_annotations(name: &str, file: &str, class: &str, decls: &[&str], src: &str) {
    let Some((krusty_dir, kotlinc_dir)) = compile_both(name, file, src) else {
        eprintln!("skip ({name}: provisioned kotlinc/JAVA_HOME unavailable)");
        return;
    };
    let krusty = annotation_shapes(&krusty_dir, class);
    let kotlinc = annotation_shapes(&kotlinc_dir, class);
    for wanted in decls {
        let pick = |shapes: &std::collections::HashMap<String, Vec<String>>| {
            let mut hits: Vec<_> = shapes
                .iter()
                .filter(|(declaration, _)| declaration.contains(wanted))
                .map(|(declaration, lines)| format!("{declaration}\n{}", lines.join("\n")))
                .collect();
            hits.sort();
            assert!(
                !hits.is_empty(),
                "{name}: no declaration matching '{wanted}'"
            );
            hits.join("\n")
        };
        assert_eq!(
            pick(&krusty),
            pick(&kotlinc),
            "{name}: '{wanted}' annotations must match kotlinc's"
        );
    }
}

#[test]
fn hidden_deprecation_matches_kotlinc() {
    // The whole shape a consumer reads back: the annotation with its NAMED `level` argument, the
    // classic `Deprecated` attribute, and ACC_SYNTHETIC. kotlinc emits no nullability annotation
    // on a synthetic declaration, so the comparison also pins that omission.
    assert_same_annotations(
        "hidden",
        "Hid.kt",
        "q/HidKt",
        &["hidden(java.lang.String)", "old(java.lang.String)"],
        "package q\n\
         @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
         fun hidden(x: String): String = x\n\
         @Deprecated(\"old\")\n\
         fun old(y: String): String = y\n",
    );
}

#[test]
fn named_and_positional_annotation_arguments_match_kotlinc() {
    // Kotlin allows a positional argument AFTER a named one when it lands in its own position.
    // Binding by label must not leave the positional cursor behind, or `3` lands on `b` a second
    // time and `c` goes missing.
    assert_same_annotations(
        "argorder",
        "Args.kt",
        "q/ArgsKt",
        &["mixed()", "allNamed()"],
        "package q\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         @Target(AnnotationTarget.FUNCTION)\n\
         annotation class Three(val a: Int, val b: Int, val c: Int)\n\
         @Three(1, b = 2, 3)\n\
         fun mixed(): String = \"x\"\n\
         @Three(c = 3, a = 1, b = 2)\n\
         fun allNamed(): String = \"x\"\n",
    );
}

#[test]
fn member_and_constructor_annotations_match_kotlinc() {
    assert_same_annotations(
        "members",
        "Mem.kt",
        "q/Holder",
        &[
            "Holder(java.lang.String, java.lang.String)",
            "hiddenMember(java.lang.String)",
            "visibleMember(java.lang.String)",
        ],
        "package q\n\
         class Holder(val tag: String) {\n\
         \x20   @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
         \x20   constructor(a: String, b: String) : this(a + b)\n\
         \x20   @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
         \x20   fun hiddenMember(v: String): String = v\n\
         \x20   fun visibleMember(v: String): String = v\n\
         }\n",
    );
}

#[test]
fn vararg_secondary_constructor_is_callable_through_its_own_metadata() {
    // The metadata record, not the class file, decides whether a consumer sees `vararg`: without
    // `ValueParameter.vararg_element_type` the parameter reads as a plain array and a call with
    // extra arguments resolves to nothing. Consuming a krusty-built library proves the record.
    const LIB: &str = "package lib\n\
        class Handle internal constructor(val names: List<String>) {\n\
        \x20   constructor(packageName: String, vararg simpleNames: String) :\n\
        \x20       this(listOf(packageName) + simpleNames)\n\
        \x20   val display: String get() = names.joinToString(\".\")\n\
        }\n";
    const MAIN: &str = "import lib.Handle\n\
        fun box(): String {\n\
        \x20   val h = Handle(\"com.example\", \"Outer\", \"Inner\")\n\
        \x20   return if (h.display == \"com.example.Outer.Inner\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(libout) = common::compile_lib("annovar", LIB) else {
        eprintln!("skip (toolchain unavailable)");
        return;
    };
    assert_eq!(
        common::compile_and_run_box(MAIN, "Main", &[libout, stdlib], Some(jdk.as_path()))
            .expect("vararg secondary ctor must be callable through krusty-built metadata"),
        "OK"
    );
}

#[test]
fn annotated_facade_constant_pool_matches_kotlinc() {
    // kotlinc's writer visits a method's header — name, descriptor, its own annotations, then the
    // nullability types — before the body. Interning a declared annotation's payload when the
    // annotation is ATTACHED (after code generation) puts every constant it introduces behind the
    // body's, so the pool diverges on every annotated method even though the attributes read the
    // same. `Deprecated` likewise interns with the method that carries it.
    let Some((krusty_dir, kotlinc_dir)) = compile_both(
        "pool",
        "Pool.kt",
        "package q\n\
         @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
         fun hidden(x: String): String = x\n\
         @Deprecated(\"old\")\n\
         fun old(y: String): String = y\n",
    ) else {
        eprintln!("skip (pool: provisioned kotlinc/JAVA_HOME unavailable)");
        return;
    };
    assert_eq!(
        constant_pool(&krusty_dir, "q/PoolKt"),
        constant_pool(&kotlinc_dir, "q/PoolKt"),
        "pool: krusty's constant pool must match kotlinc's for an annotated facade"
    );
}

#[test]
fn binary_retained_annotation_precedes_the_nullability_one() {
    // A BINARY-retained declared annotation shares `RuntimeInvisibleAnnotations` with the
    // compiler's own `@NotNull` on the return. kotlinc writes the DECLARED one first, whichever
    // order the emitter's two setters ran in.
    assert_same_annotations(
        "binret",
        "Bin.kt",
        "q/BinKt",
        &["marked(java.lang.String)"],
        "package q\n\
         @Retention(AnnotationRetention.BINARY)\n\
         @Target(AnnotationTarget.FUNCTION)\n\
         annotation class Keep(val why: String)\n\
         @Keep(\"reason\")\n\
         fun marked(v: String): String = v\n",
    );
}

#[test]
fn nested_annotation_argument_matches_kotlinc() {
    assert_same_annotations(
        "nested",
        "Nest.kt",
        "q/NestKt",
        &["older(java.lang.String)"],
        "package q\n\
         @Deprecated(\"gone\", ReplaceWith(\"newer(x)\"), DeprecationLevel.HIDDEN)\n\
         fun older(x: String): String = x\n\
         fun newer(x: String): String = x\n",
    );
}

#[test]
fn imported_class_literal_annotation_argument_matches_kotlinc() {
    assert_same_annotations(
        "imported_class_literal",
        "ClassLiteral.kt",
        "q/ClassLiteralKt",
        &["marked()"],
        "package q\n\
         import kotlin.reflect.KClass\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         @Target(AnnotationTarget.FUNCTION)\n\
         annotation class ClassValue(val value: KClass<*>)\n\
         @ClassValue(String::class)\n\
         fun marked(): String = \"OK\"\n",
    );
}

#[test]
fn argument_bearing_function_annotation_round_trips_through_krusty_metadata() {
    const LIB: &str = "package lib\n\
        import kotlin.jvm.JvmName\n\
        @JvmName(\"physicalName\")\n\
        fun sourceName(): String = \"OK\"\n";
    const MAIN: &str = "import lib.sourceName\n\
        fun box(): String = sourceName()\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(library) = common::compile_lib("annotation-metadata-args", LIB) else {
        eprintln!("skip (toolchain unavailable)");
        return;
    };
    assert_eq!(
        common::compile_and_run_box(MAIN, "Main", &[library, stdlib], Some(jdk.as_path()))
            .expect("argument-bearing @JvmName must survive Krusty metadata round-trip"),
        "OK",
    );
}

#[test]
fn missing_required_annotation_argument_is_a_frontend_error() {
    let diagnostics = common::front_end_diagnostics(
        "annotation class Required(val value: String)\n\
         @Required()\n\
         fun marked(): String = \"bad\"\n",
        &[],
        None,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("no value passed for parameter 'value'")),
        "missing required annotation element must not reach lowering: {diagnostics:?}"
    );
}
