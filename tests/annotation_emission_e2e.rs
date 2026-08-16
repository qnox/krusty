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
//!
//! The class file's attribute is only half of it: kotlinc ALSO mirrors a declaration's annotations
//! into `@kotlin.Metadata`, which is where a Kotlin consumer reads them back from. The
//! `…_reaches_metadata` cases below compare that payload (`d1`/`d2`) directly.
use std::fs;

use super::common;

/// Per-DECLARATION annotation facts from `javap -v`, keyed by the declaration line: its flags and
/// its annotation attributes, with constant-pool indices stripped (`#12` → `#`). Class-level
/// attributes (notably `@Metadata`) are excluded — the metadata mirror of these same annotations is
/// compared separately (`metadata_payload`), and folding it in here would mask which of the two
/// halves diverged.
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
/// `@Metadata` payload (a `Utf8` whose text starts with a NUL), which `metadata_payload` compares on
/// its own terms. Pool ORDER is what the annotation-reservation and attribute-name interning fixes
/// are about, so this compares the sequence, not a set.
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
    compile_both_with(name, file, src, None)
}

/// [`compile_both`] against an extra classpath entry — the Java `@interface` fixtures live there,
/// and their element facts (`AnnotationDefault`, a `byte`/`short` element's tag) cannot be
/// expressed by a Kotlin annotation declared in the source under test.
fn compile_both_with(
    name: &str,
    file: &str,
    src: &str,
    library: Option<&std::path::Path>,
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
    let mut kotlinc_args = vec![
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ];
    if let Some(library) = library {
        kotlinc_args.push("-cp".to_string());
        kotlinc_args.push(library.to_string_lossy().to_string());
    }
    let (code, stderr) = common::kotlinc_compile(&kotlinc_args)?;
    assert_eq!(code, 0, "{name}: kotlinc rejected the fixture: {stderr}");

    let stem = file.strip_suffix(".kt").expect("fixture is a .kt file");
    let mut classpath = vec![stdlib];
    if let Some(library) = library {
        classpath.push(library.to_path_buf());
    }
    let classes =
        common::compile_in_process(src, stem, &classpath, Some(common::jdk_modules().as_path()))
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

fn assert_compiled_annotations(
    name: &str,
    class: &str,
    decls: &[&str],
    krusty_dir: &std::path::Path,
    kotlinc_dir: &std::path::Path,
) {
    let krusty = annotation_shapes(krusty_dir, class);
    let kotlinc = annotation_shapes(kotlinc_dir, class);
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

/// Compare the annotation facts of every declaration whose javap line contains one of `decls`.
/// Selecting declarations keeps the assertion independent of the order krusty emits methods in,
/// which is a separate parity concern.
fn assert_same_annotations(name: &str, file: &str, class: &str, decls: &[&str], src: &str) {
    let Some((krusty_dir, kotlinc_dir)) = compile_both(name, file, src) else {
        eprintln!("skip ({name}: provisioned kotlinc/JAVA_HOME unavailable)");
        return;
    };
    assert_compiled_annotations(name, class, decls, &krusty_dir, &kotlinc_dir);
}

/// A regression whose only oracle is kotlinc must not turn green without running that oracle.
fn require_same_annotations(name: &str, file: &str, class: &str, decls: &[&str], src: &str) {
    let (krusty_dir, kotlinc_dir) = compile_both(name, file, src)
        .unwrap_or_else(|| panic!("{name}: provisioned kotlinc/JAVA_HOME unavailable"));
    assert_compiled_annotations(name, class, decls, &krusty_dir, &kotlinc_dir);
}

fn require_same_annotations_with(
    name: &str,
    file: &str,
    class: &str,
    decls: &[&str],
    src: &str,
    library: Option<&std::path::Path>,
) {
    let (krusty_dir, kotlinc_dir) = compile_both_with(name, file, src, library)
        .unwrap_or_else(|| panic!("{name}: provisioned kotlinc/JAVA_HOME unavailable"));
    assert_compiled_annotations(name, class, decls, &krusty_dir, &kotlinc_dir);
}

/// The class's `@kotlin.Metadata` payload — the `d1`/`d2` lines of `javap -v`, verbatim. This is the
/// Kotlin-level MIRROR of an annotation: the class file's `RuntimeVisibleAnnotations` attribute makes
/// the annotation work at runtime, while a Kotlin consumer (and `kotlin-reflect`) reads the applied
/// annotations back out of these two arrays.
fn metadata_payload(dir: &std::path::Path, class: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-v", "-p", &path.to_string_lossy()]).expect("pooled javap");
    let payload: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("d1=") || line.starts_with("d2="))
        .map(str::to_string)
        .collect();
    assert_eq!(payload.len(), 2, "{class} carries a @Metadata d1 and d2");
    payload
}

/// A declaration's annotations must reach `@Metadata`, not only the class file's annotation
/// attributes — compared against kotlinc's own payload, which pins the record's field numbers, the
/// `HAS_ANNOTATIONS` flag bit, and the position the annotation's class id takes in the string table.
fn require_same_metadata(name: &str, file: &str, class: &str, src: &str) {
    let (krusty_dir, kotlinc_dir) = compile_both(name, file, src)
        .unwrap_or_else(|| panic!("{name}: provisioned kotlinc/JAVA_HOME unavailable"));
    assert_eq!(
        metadata_payload(&krusty_dir, class),
        metadata_payload(&kotlinc_dir, class),
        "{name}: {class}'s @Metadata must record the applied annotations exactly as kotlinc does"
    );
}

/// The whole class file must be byte-for-byte identical to kotlinc's — the strongest form, available
/// when nothing but the metadata mirror separated the two.
fn require_identical_class(name: &str, file: &str, class: &str, src: &str) {
    require_identical_class_with(name, file, class, src, None)
}

/// [`require_identical_class`] against an extra classpath entry — a Java `@interface` fixture, whose
/// `@Target` only the class file states.
fn require_identical_class_with(
    name: &str,
    file: &str,
    class: &str,
    src: &str,
    library: Option<&std::path::Path>,
) {
    let (krusty_dir, kotlinc_dir) = compile_both_with(name, file, src, library)
        .unwrap_or_else(|| panic!("{name}: provisioned kotlinc/JAVA_HOME unavailable"));
    let read = |dir: &std::path::Path| {
        fs::read(dir.join(format!("{class}.class")))
            .unwrap_or_else(|e| panic!("{name}: reading {class}.class: {e}"))
    };
    let (krusty, kotlinc) = (read(&krusty_dir), read(&kotlinc_dir));
    assert_eq!(
        metadata_payload(&krusty_dir, class),
        metadata_payload(&kotlinc_dir, class),
        "{name}: {class}'s @Metadata must match kotlinc's"
    );
    assert_eq!(
        krusty,
        kotlinc,
        "{name}: {class} must be byte-for-byte identical to kotlinc (krusty {} B, kotlinc {} B)",
        krusty.len(),
        kotlinc.len(),
    );
}

/// An annotated MEMBER FUNCTION: the class file's `RuntimeVisibleAnnotations` always carried it, but
/// the `@Metadata` mirror did not, so a Kotlin consumer reading the declaration back saw an
/// unannotated function. kotlinc records `Function.annotation` (f12) plus the `HAS_ANNOTATIONS` flag
/// bit; with those the whole class file matches.
#[test]
fn member_function_annotation_reaches_metadata() {
    require_identical_class(
        "member_fn_anno",
        "A.kt",
        "p/C",
        "package p\n\n\
         annotation class Mark\n\n\
         class C {\n\
         \x20   @Mark\n\
         \x20   fun f(): Int = 1\n\
         }\n",
    );
}

/// The same mirror with ARGUMENTS and a second annotation: the record carries the element values
/// (`Annotation.argument`), so the ORDER the values intern their strings in is pinned too.
#[test]
fn member_function_annotation_arguments_reach_metadata() {
    require_same_metadata(
        "member_fn_anno_args",
        "A.kt",
        "p/C",
        "package p\n\n\
         annotation class Mark(val tag: String, val level: Int)\n\n\
         annotation class Plain\n\n\
         class C {\n\
         \x20   @Mark(tag = \"t\", level = 2)\n\
         \x20   @Plain\n\
         \x20   fun f(): Int = 1\n\
         }\n",
    );
}

/// A BINARY-retained annotation reaches the same record. The class file splits the two retentions
/// into different attributes (`RuntimeInvisibleAnnotations` here); `@Metadata` keeps ONE list, so
/// mirroring only the runtime-visible half would lose the declaration kotlinc records.
#[test]
fn binary_retained_member_annotation_reaches_metadata() {
    require_same_metadata(
        "member_fn_binary_anno",
        "A.kt",
        "p/C",
        "package p\n\n\
         @Retention(AnnotationRetention.BINARY)\n\
         annotation class Mark\n\n\
         class C {\n\
         \x20   @Mark\n\
         \x20   fun f(): Int = 1\n\
         }\n",
    );
}

/// An annotated PROPERTY. Kotlin has no `property` in the class file, so kotlinc parks the
/// annotation on a synthetic `getV$annotations()V` marker method and names it from
/// `JvmPropertySignature.syntheticMethod` — the annotation is not on the field and not on the getter.
/// krusty dropped it from both the class file and the metadata.
#[test]
fn property_annotation_becomes_a_synthetic_marker_method() {
    require_identical_class(
        "prop_anno",
        "A.kt",
        "p/C",
        "package p\n\n\
         annotation class Mark\n\n\
         class C {\n\
         \x20   @Mark\n\
         \x20   val v: Int = 1\n\
         }\n",
    );
}

/// An annotated `var` with ARGUMENTS: the marker carries the element values, and the property's
/// SETTER flags word is written out beside the getter's (both differ from the default kotlinc
/// derives from an annotated property). Two annotated properties also pin each marker's position —
/// it follows its own getter, not the last one.
#[test]
fn annotated_var_and_second_property_keep_their_own_markers() {
    require_identical_class(
        "prop_anno_var",
        "A.kt",
        "p/C",
        "package p\n\n\
         annotation class Mark(val tag: String)\n\n\
         class C {\n\
         \x20   @Mark(\"a\")\n\
         \x20   var first: Int = 1\n\
         \x20   @Mark(\"b\")\n\
         \x20   val second: String = \"s\"\n\
         }\n",
    );
}

/// `@Target` written as a NAMED array (`allowedTargets = [...]`) — the only spelling that can list
/// more than one target — must resolve to the same target set as the positional vararg one. Reading
/// only the vararg form left the set empty, which drops the annotation from BOTH use sites.
#[test]
fn array_spelled_target_still_places_the_annotation() {
    require_identical_class(
        "prop_anno_array_target",
        "A.kt",
        "p/C",
        "package p\n\n\
         @Target(allowedTargets = [AnnotationTarget.FIELD, AnnotationTarget.VALUE_PARAMETER])\n\
         annotation class Mark\n\n\
         class C {\n\
         \x20   @Mark\n\
         \x20   val v: Int = 1\n\
         }\n",
    );
}

/// A FIELD-targeted annotation on the same declaration takes the other route: Kotlin's use-site
/// default picks the backing field, so the annotation lands on the field and the metadata records it
/// in the property's field-annotation list — no marker method exists.
#[test]
fn field_targeted_property_annotation_lands_on_the_backing_field() {
    require_identical_class(
        "prop_field_anno",
        "A.kt",
        "p/C",
        "package p\n\n\
         @Target(AnnotationTarget.FIELD)\n\
         annotation class Mark\n\n\
         class C {\n\
         \x20   @Mark\n\
         \x20   val v: Int = 1\n\
         }\n",
    );
}

/// A primary-constructor `val` is a property too, but its annotations default to the PARAMETER — so
/// only an annotation that cannot target a value parameter reaches the property (or the field). Both
/// non-parameter routes are exercised here; the plain `@Mark` case stays a parameter annotation.
#[test]
fn constructor_property_annotations_follow_their_declared_target() {
    require_identical_class(
        "ctor_prop_anno",
        "A.kt",
        "p/C",
        "package p\n\n\
         @Target(AnnotationTarget.PROPERTY)\n\
         annotation class OnProperty\n\n\
         @Target(AnnotationTarget.FIELD)\n\
         annotation class OnField\n\n\
         class C(@OnProperty val a: Int, @OnField val b: Int)\n",
    );
}

/// A declaration carrying BOTH an annotation and a `JvmMethodSignature` (a `suspend` member, whose
/// CPS descriptor the proto types do not imply) pins the two ORDERS apart: the signature interns its
/// strings FIRST, while the annotation serializes first (f12 before f100). Getting that backwards
/// swaps two d2 entries and every index after them.
#[test]
fn annotated_suspend_member_interns_its_signature_before_the_annotation() {
    require_same_metadata(
        "member_suspend_anno",
        "A.kt",
        "p/C",
        "package p\n\n\
         annotation class Mark\n\n\
         class C {\n\
         \x20   @Mark\n\
         \x20   suspend fun f(a: Int): Int = a\n\
         }\n",
    );
}

/// The same order through the FACADE's package metadata, which a separate serializer builds.
#[test]
fn annotated_suspend_top_level_function_interns_its_signature_before_the_annotation() {
    require_same_metadata(
        "facade_suspend_anno",
        "A.kt",
        "p/AKt",
        "package p\n\n\
         annotation class Mark\n\n\
         @Mark\n\
         suspend fun f(a: Int): Int = a\n",
    );
}

/// An annotated SECONDARY CONSTRUCTOR — `Constructor.annotation` is its own field (f3), and the
/// constructor's flags word gains the same `HAS_ANNOTATIONS` bit (22 → 23).
#[test]
fn secondary_constructor_annotation_reaches_metadata() {
    require_same_metadata(
        "ctor_anno",
        "A.kt",
        "p/C",
        "package p\n\n\
         annotation class Mark\n\n\
         class C(val x: Int) {\n\
         \x20   @Mark\n\
         \x20   constructor() : this(1)\n\
         }\n",
    );
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

#[test]
fn nested_annotation_named_arguments_bind_by_label() {
    // A NESTED annotation is parsed as an ordinary call, so its labels live in the call's own
    // argument names rather than the annotation's. Reading only the latter bound every nested
    // argument positionally, which silently SWAPPED the values whenever the labels were written
    // out of declaration order — and, when the elements had different types, reported two
    // spurious argument-type mismatches instead.
    require_same_annotations(
        "nested-named",
        "Nested.kt",
        "q/Holder3",
        &["tagged()", "mixed()", "deep()", "typed()"],
        "package q\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         annotation class Innermost(val x: Int, val y: Int)\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         annotation class Typed(val n: Int, val a: String)\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         annotation class Deep(val m: Innermost, val a: String)\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         annotation class Inner(val a: String, val b: String)\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         @Target(AnnotationTarget.FUNCTION)\n\
         annotation class Outer(val i: Inner)\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         @Target(AnnotationTarget.FUNCTION)\n\
         annotation class OuterDeep(val d: Deep)\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         @Target(AnnotationTarget.FUNCTION)\n\
         annotation class OuterTyped(val t: Typed)\n\
         class Holder3 {\n\
         \x20   @Outer(Inner(b = \"BB\", a = \"AA\"))\n\
         \x20   fun tagged() {}\n\
         \x20   @Outer(Inner(\"AA\", b = \"BB\"))\n\
         \x20   fun mixed() {}\n\
         \x20   @OuterDeep(Deep(m = Innermost(y = 1, x = 2), a = \"AA\"))\n\
         \x20   fun deep() {}\n\
         \x20   @OuterTyped(Typed(a = \"AA\", n = 7))\n\
         \x20   fun typed() {}\n\
         }\n",
    );
}

/// A Java `@interface` on the classpath, compiled by javac: `AnnotationDefault`, an array-typed
/// `value`, and a second array element that has no shorthand.
fn java_annotation_library() -> Option<std::path::PathBuf> {
    let java = [
        (
            "Sched.java".into(),
            "package jl;\n\
             import java.lang.annotation.*;\n\
             @Retention(RetentionPolicy.RUNTIME)\n\
             @Target({ElementType.METHOD})\n\
             public @interface Sched {\n\
             \x20   String fixedDelay() default \"\";\n\
             \x20   String cron() default \"\";\n\
             \x20   String zoneId() default \"\";\n\
             }\n"
            .into(),
        ),
        (
            "Filt.java".into(),
            "package jl;\n\
             import java.lang.annotation.*;\n\
             @Retention(RetentionPolicy.RUNTIME)\n\
             @Target({ElementType.TYPE})\n\
             public @interface Filt {\n\
             \x20   String[] value() default {};\n\
             \x20   String[] tags() default {};\n\
             }\n"
            .into(),
        ),
        (
            "Named.java".into(),
            "package jl;\n\
             import java.lang.annotation.*;\n\
             @Retention(RetentionPolicy.RUNTIME)\n\
             @Target({ElementType.TYPE})\n\
             public @interface Named {\n\
             \x20   String text();\n\
             }\n"
            .into(),
        ),
        (
            "Single.java".into(),
            "package jl;\n\
             import java.lang.annotation.*;\n\
             @Retention(RetentionPolicy.RUNTIME)\n\
             @Target({ElementType.TYPE, ElementType.METHOD})\n\
             public @interface Single {\n\
             \x20   String value();\n\
             \x20   String other() default \"\";\n\
             }\n"
            .into(),
        ),
    ];
    common::javac_compile(&java, &[]).map(|(dir, _)| dir)
}

#[test]
fn a_java_annotation_without_value_requires_named_arguments() {
    let library = java_annotation_library().expect("javac must build the Java annotation fixture");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library.clone(), stdlib];
    let accepted = "import jl.Named\n@Named(text = \"ok\")\nclass Accepted\n";
    assert!(
        common::front_end_diagnostics(accepted, &classpath, Some(jdk.as_path())).is_empty(),
        "named Java annotation argument must be accepted"
    );

    let rejected = "import jl.Named\n@Named(\"bad\")\nclass Rejected\n";
    let diagnostics = common::front_end_diagnostics(rejected, &classpath, Some(jdk.as_path()));
    assert_eq!(
        diagnostics,
        ["only named arguments are available for Java annotations."]
    );

    let base = std::env::temp_dir().join(format!(
        "krusty_java_annotation_named_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    let output = base.join("out");
    fs::create_dir_all(&output).unwrap();
    let source = base.join("Rejected.kt");
    fs::write(&source, rejected).unwrap();
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().into_owned(),
        "-cp".to_string(),
        library.to_string_lossy().into_owned(),
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
    ])
    .expect("provisioned kotlinc must check the Java annotation fixture");
    assert_ne!(
        code, 0,
        "kotlinc unexpectedly accepted positional Java annotation argument"
    );
    assert!(
        stderr.contains("only named arguments are available for Java annotations"),
        "unexpected kotlinc diagnostic: {stderr}"
    );
}

#[test]
fn a_java_scalar_value_accepts_only_its_own_positional_argument() {
    let library = java_annotation_library().expect("javac must build the Java annotation fixture");
    require_same_annotations_with(
        "java-scalar-value",
        "Scalar.kt",
        "ScalarAccepted",
        &["marked()"],
        "import jl.Single\n\
         class ScalarAccepted {\n\
         \x20   @Single(\"ok\", other = \"yes\")\n\
         \x20   fun marked() {}\n\
         }\n",
        Some(library.as_path()),
    );

    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library.clone(), stdlib];
    let rejected = "import jl.Single\n@Single(\"ok\", \"bad\")\nclass Rejected\n";
    assert_eq!(
        common::front_end_diagnostics(rejected, &classpath, Some(jdk.as_path())),
        ["only named arguments are available for Java annotations."]
    );

    let base = std::env::temp_dir().join(format!(
        "krusty_java_annotation_scalar_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    let output = base.join("out");
    fs::create_dir_all(&output).unwrap();
    let source = base.join("Rejected.kt");
    fs::write(&source, rejected).unwrap();
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().into_owned(),
        "-cp".to_string(),
        library.to_string_lossy().into_owned(),
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
    ])
    .expect("provisioned kotlinc must check scalar Java annotation arguments");
    assert_ne!(
        code, 0,
        "kotlinc unexpectedly accepted a second positional argument"
    );
    assert!(
        stderr.contains("only named arguments are available for Java annotations"),
        "unexpected kotlinc diagnostic: {stderr}"
    );
}

#[test]
fn unsupported_annotation_values_are_reported_only_at_consumed_positions() {
    let source = "@Target(AnnotationTarget.CLASS, AnnotationTarget.PROPERTY, AnnotationTarget.FIELD, AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.LOCAL_VARIABLE)\n\
        annotation class Meta(val tags: Array<String>)\n\
        @Meta(tags = [\"class\"]) class Rejected(\n\
        \x20   @Meta(tags = [\"parameter\"]) val parameter: String\n\
        ) {\n\
        \x20   @field:Meta(tags = [\"field\"])\n\
        \x20   @Meta(tags = [\"property\"])\n\
        \x20   val property: String = parameter\n\
        \x20   fun keep(@Meta(tags = [\"value-parameter\"]) value: String): String {\n\
        \x20       @Meta(tags = [\"local\"]) val local = value\n\
        \x20       return local\n\
        \x20   }\n\
        }\n";
    let (code, stderr) = common::kotlinc_source_result("AnnotationConsumption", source);
    assert_eq!(code, 0, "kotlinc rejected annotation positions: {stderr}");
    let diagnostics = common::front_end_diagnostics_with_stdlib(source);
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "an array-literal argument is checked against its declared element type, so every one of \
         these positions compiles as it does under kotlinc"
    );
}

#[test]
fn a_java_annotation_element_default_may_be_omitted() {
    // `AnnotationDefault` is the ONLY carrier of a Java annotation element's default — a Java
    // `@interface` has no Kotlin constructor to hold them. Ignoring the attribute made every
    // unsupplied element a "no value passed for parameter" error, which gated whole modules on
    // shapes as ordinary as `@Scheduled(fixedDelay = "1m")`.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = java_annotation_library().expect("javac must build the Java annotation fixture");
    let classpath = vec![library, stdlib];
    const MAIN: &str = "import jl.Sched\n\
        class Worker {\n\
        \x20   @Sched(fixedDelay = \"1m\")\n\
        \x20   fun tick() {}\n\
        }\n\
        fun box(): String = \"OK\"\n";
    let classes = common::compile_in_process(MAIN, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(MAIN, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}

#[test]
fn a_java_annotation_array_value_is_a_positional_vararg_only() {
    // Measured against kotlinc 2.4.10, which is stricter than "arrays accept a single value":
    // only the `value` element is a vararg, and only POSITIONALLY. `value = "a"` and any other
    // array element still demand an array literal, so accepting those would compile what kotlinc
    // rejects.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = java_annotation_library().expect("javac must build the Java annotation fixture");
    let classpath = vec![library, stdlib];
    let source = |application: &str| {
        format!("import jl.Filt\n{application}\nclass Target\nfun box(): String = \"OK\"\n")
    };
    for accepted in ["@Filt(\"a\")", "@Filt(\"a\", \"b\")"] {
        let main = source(accepted);
        assert!(
            common::compile_in_process(&main, "Main", &classpath, Some(jdk.as_path())).is_some(),
            "kotlinc accepts {accepted}: {:?}",
            common::front_end_diagnostics(&main, &classpath, Some(jdk.as_path()))
        );
    }
    // An array LITERAL argument retains its elements until the checker can fold them against the
    // DECLARED element type, so these compile like kotlinc.
    for accepted in [
        "@Filt(\"a\", tags = [\"t\"])",
        "@Filt(value = [\"a\"], tags = [\"t\"])",
    ] {
        let main = source(accepted);
        assert!(
            common::compile_in_process(&main, "Main", &classpath, Some(jdk.as_path())).is_some(),
            "kotlinc accepts {accepted}: {:?}",
            common::front_end_diagnostics(&main, &classpath, Some(jdk.as_path()))
        );
    }
    for rejected in ["@Filt(tags = \"t\")", "@Filt(value = \"a\")"] {
        let main = source(rejected);
        let diagnostics = common::front_end_diagnostics(&main, &classpath, Some(jdk.as_path()));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("argument type mismatch")),
            "kotlinc rejects {rejected}, so krusty must too: {diagnostics:?}"
        );
    }
}

#[test]
fn array_literal_annotation_tags_match_kotlinc() {
    let java = [(
        "Arrays.java".into(),
        "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.METHOD})\n\
         public @interface Arrays {\n\
         \x20   byte[] bytes(); short[] shorts(); int[] ints(); long[] longs();\n\
         \x20   float[] floats(); double[] doubles(); char[] chars(); boolean[] booleans();\n\
         \x20   String[] strings(); Class<?>[] classes();\n\
         }\n"
        .into(),
    )];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must compile the array annotation fixture");
    const SOURCE: &str = "import jl.Arrays\n\
        class Tagged {\n\
        \x20 @Arrays(\n\
        \x20 bytes = [1], shorts = [2], ints = [3], longs = [4L],\n\
        \x20 floats = [5.0f], doubles = [6.0], chars = ['x'], booleans = [true],\n\
        \x20 strings = [\"s\"], classes = [String::class])\n\
        \x20 fun marked() {}\n\
        }\n";

    require_same_annotations_with(
        "array_literal_tags",
        "Arrays.kt",
        "Tagged",
        &["marked()"],
        SOURCE,
        Some(&library),
    );
}

#[test]
fn a_classpath_const_val_is_an_annotation_constant() {
    // A qualified read in an annotation argument is an enum entry OR a named constant. Folding
    // only the enum case rejected `@Guarded(Rules.AUTHENTICATED)` — a `const val` reached through
    // its owner — as "not a supported compile-time constant".
    const LIB: &str = "package lib\n\
        interface Rules {\n\
        \x20   companion object { const val AUTHENTICATED = \"isAuthenticated()\" }\n\
        }\n\
        @Target(AnnotationTarget.CLASS)\n\
        @Retention(AnnotationRetention.RUNTIME)\n\
        annotation class Guarded(vararg val value: String)\n";
    const MAIN: &str = "import lib.Guarded\n\
        import lib.Rules\n\
        @Guarded(Rules.AUTHENTICATED)\n\
        class Service\n\
        fun box(): String = \"OK\"\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = common::compile_lib("ann-const", LIB)
        .expect("krusty must build the classpath annotation fixture");
    let classpath = vec![library, stdlib];
    let classes = common::compile_in_process(MAIN, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(MAIN, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}

#[test]
fn java_element_tags_and_omitted_defaults_match_kotlinc() {
    // Three facts a "does it compile" assertion cannot see, all differential against kotlinc:
    //   * an OMITTED Java element emits nothing, so the classfile's own `AnnotationDefault`
    //     stands — materializing `kept=[]` would override it;
    //   * a `byte`/`short` element carries its own tag (`B`, `S`). The descriptor boundary widens
    //     both to `Int` for overload resolution, and borrowing that tag throws
    //     `AnnotationTypeMismatchException` when the annotation is read back;
    //   * a NAMED vararg keeps the element expectation inside its array (`[Z`, not `[I`).
    let java = [(
        "Tags.java".into(),
        "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.METHOD})\n\
         public @interface Tags {\n\
         \x20   String[] kept() default {\"d\"};\n\
         \x20   byte by() default 0;\n\
         \x20   short sh() default 0;\n\
         \x20   boolean[] flags() default {};\n\
         }\n"
        .into(),
    )];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must build the Java annotation fixture");
    require_same_annotations_with(
        "javatags",
        "Tagged.kt",
        "Holder",
        &["tagged()"],
        "import jl.Tags\n\
         class Holder {\n\
         \x20   @Tags(by = 3, sh = 9, flags = booleanArrayOf(true, false))\n\
         \x20   fun tagged() {}\n\
         }\n",
        Some(library.as_path()),
    );
}

#[test]
fn omitted_java_default_and_named_vararg_constants_match_kotlinc() {
    // The two guards the tag comparison above cannot reach, because its fixture has no element
    // named `value`:
    //   * an OMITTED element on an annotation that HAS a `value` vararg must still emit nothing,
    //     so the classfile's own `AnnotationDefault` stands — materializing `value=[]` overrides
    //     a non-empty default outright;
    //   * a NAMED `value` fed CONSTANTS keeps the element expectation inside the array. Literals
    //     cannot show this: they carry their own type, while a `const val` folds through the
    //     expected type and lands as `I` without the guard.
    let java = [(
        "Vals.java".into(),
        "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.METHOD})\n\
         public @interface Vals {\n\
         \x20   boolean[] value() default {};\n\
         \x20   String[] kept() default {\"d\"};\n\
         }\n"
        .into(),
    )];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must build the Java annotation fixture");
    require_same_annotations_with(
        "javavals",
        "Vals.kt",
        "Holder2",
        &["omitted()", "constants()"],
        "import jl.Vals\n\
         object K {\n\
         \x20   const val T = true\n\
         \x20   const val F = false\n\
         }\n\
         class Holder2 {\n\
         \x20   @Vals\n\
         \x20   fun omitted() {}\n\
         \x20   @Vals(value = booleanArrayOf(K.T, K.F))\n\
         \x20   fun constants() {}\n\
         }\n",
        Some(library.as_path()),
    );
}

/// A CLASSPATH annotation's `@Target` must be read from its class file, not assumed. A Java
/// `@interface` restricted to `ElementType.FIELD` — the shape every field-oriented Java framework
/// annotation has — cannot target a Kotlin PROPERTY at all, so kotlinc puts it on the backing field.
/// Assuming the Kotlin default (applicable everywhere) instead parks it on the property's synthetic
/// `$annotations` marker, where nothing reflecting over fields will ever see it.
#[test]
fn a_field_targeted_java_annotation_lands_on_the_backing_field() {
    let java = [(
        "OnField.java".into(),
        "package jl;
         import java.lang.annotation.*;
         @Retention(RetentionPolicy.RUNTIME)
         @Target({ElementType.FIELD})
         public @interface OnField {}
"
        .into(),
    )];
    let (library, _) = common::javac_compile(&java, &[])
        .expect("javac must build the field-targeted annotation fixture");
    require_identical_class_with(
        "java_field_target",
        "A.kt",
        "p/C",
        "package p

         import jl.OnField

         class C {
             @OnField
             val v: Int = 1
         }
",
        Some(library.as_path()),
    );
}
