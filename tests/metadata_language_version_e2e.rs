//! `-Xmetadata-version X.Y` independently overrides every emitted `@kotlin.Metadata` `mv` and the
//! `META-INF/<module>.kotlin_module` version header with `[X, Y, 0]`. The standard public
//! `-language-version` selects that same default stamp through the CLI; these tests isolate the
//! backend-only override. The reference kotlinc's no-flag stamp is `[2, 4, 0]`, and its
//! `-language-version 2.2` stamp is the value the internal override is compared against.

use super::common;

const SRC: &str = "package app\n\
    \n\
    import kotlin.coroutines.suspendCoroutine\n\
    \n\
    interface Face {\n\
    \x20   fun n(x: Int = 1): Int = x\n\
    }\n\
    \n\
    class Holder(val value: Int) {\n\
    \x20   fun doubled() = value * 2\n\
    }\n\
    \n\
    fun topLevel(x: Int): Int = Holder(x).doubled()\n\
    \n\
    inline fun wrapped(n: Int): Any = object { fun v() = n }\n\
    \n\
    fun call() = wrapped(1)\n\
    \n\
    suspend fun paused(): String {\n\
    \x20   suspendCoroutine<Unit> { }\n\
    \x20   return \"S\"\n\
    }\n";

/// Classes whose `@Metadata` is written by a path other than an ordinary class or file facade.
const SYNTHETIC_STAMPS: &[&str] = &[
    "app/Face$DefaultImpls",
    "app/HolderKt$paused$1",
    "app/HolderKt$wrapped$1",
];

/// The `mv` of a class's `@kotlin.Metadata`, or `None` for a class without one.
fn metadata_mv(bytes: &[u8]) -> Option<Vec<i32>> {
    super::common_core::kotlin_metadata::kotlin_metadata_ints(bytes)?
        .into_iter()
        .find(|(name, _)| name == "mv")
        .map(|(_, ints)| ints)
}

/// A `.kotlin_module` header: five big-endian i32s, `[len=3, major, minor, patch, flags]`.
fn module_header(bytes: &[u8]) -> Vec<i32> {
    bytes[..20]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| i32::from_be_bytes(*word))
        .collect()
}

/// The fixture compiled with the given `-language-version` stamp (`None` = no flag).
fn stamped(metadata_version: Option<[i32; 3]>) -> Vec<(String, Vec<u8>)> {
    common::compile_in_process_files_metadata_version(
        &[("Holder.kt", SRC)],
        &[common::stdlib_jar()],
        None,
        metadata_version,
    )
    .expect("krusty compiles the fixture")
}

/// Every emitted class that carries `@Metadata` must stamp `expected`, and so must the module
/// file's version header.
fn assert_stamped(metadata_version: Option<[i32; 3]>, expected: [i32; 3]) {
    let outputs = stamped(metadata_version);
    let mut stamped_classes = 0;
    let mut module_files = 0;
    for (name, bytes) in &outputs {
        if name.ends_with(".kotlin_module") {
            module_files += 1;
            assert_eq!(
                module_header(bytes),
                vec![3, expected[0], expected[1], expected[2], 0],
                "{name}: the module header version follows -language-version"
            );
        } else if let Some(mv) = metadata_mv(bytes) {
            stamped_classes += 1;
            assert_eq!(mv, expected.to_vec(), "{name}: @Metadata mv");
        }
    }
    assert!(
        stamped_classes >= 2,
        "the fixture's facade and class both carry @Metadata: {outputs:?}"
    );
    assert_eq!(module_files, 1, "one module file: {outputs:?}");
    let names = outputs
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    for class in SYNTHETIC_STAMPS {
        assert!(names.contains(class), "missing {class} among {names:?}");
    }
}

#[test]
fn no_flag_keeps_the_default_language_version_stamp() {
    assert_stamped(None, [2, 4, 0]);
}

#[test]
fn language_version_stamps_every_metadata_and_the_module_file() {
    assert_stamped(Some([2, 2, 0]), [2, 2, 0]);
}

/// The same fixture through kotlinc `-language-version 2.2`: every class's `mv`, including
/// `$DefaultImpls`, the suspend lambda, and the inline function's object, and the module file are
/// compared against the internal 2.2 stamp.
#[test]
fn language_version_2_2_matches_kotlinc() {
    let Some(dir) = common::scratch_dir() else {
        return;
    };
    let out = dir.join("ref");
    std::fs::create_dir_all(&out).unwrap();
    let src_path = dir.join("Holder.kt");
    std::fs::write(&src_path, SRC).unwrap();
    let args = vec![
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        "-language-version".to_string(),
        "2.2".to_string(),
        src_path.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let mut reference: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stack = vec![out.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "class") {
                let internal = path
                    .strip_prefix(&out)
                    .unwrap()
                    .to_string_lossy()
                    .trim_end_matches(".class")
                    .to_string();
                reference.push((internal, std::fs::read(&path).unwrap()));
            }
        }
    }
    let reference_module = std::fs::read(out.join("META-INF/main.kotlin_module"))
        .expect("kotlinc writes the module file");

    let actual = stamped(Some([2, 2, 0]));
    for (name, reference_bytes) in &reference {
        let (_, actual_bytes) = actual
            .iter()
            .find(|(emitted, _)| emitted == name)
            .unwrap_or_else(|| panic!("krusty did not emit {name}"));
        assert_eq!(
            metadata_mv(actual_bytes),
            metadata_mv(reference_bytes),
            "{name}: @Metadata mv under -language-version 2.2"
        );
    }
    let (_, actual_module) = actual
        .iter()
        .find(|(name, _)| name.ends_with(".kotlin_module"))
        .expect("krusty writes the module file");
    assert_eq!(
        module_header(actual_module),
        module_header(&reference_module),
        "the .kotlin_module version header under the 2.2 stamp"
    );
    for class in SYNTHETIC_STAMPS {
        assert!(
            reference.iter().any(|(name, _)| name == class),
            "kotlinc omitted {class}"
        );
    }
}

const INLINE_LIB: &str = "package lib\n\
    \n\
    interface Greeter { fun greet(): String }\n\
    \n\
    inline fun greeter(prefix: String): Greeter = object : Greeter {\n\
    \x20   override fun greet(): String = prefix\n\
    }\n";

const INLINE_CALLER: &str = "import lib.greeter\n\
    \n\
    fun box(): String = greeter(\"a\").greet()\n";

/// A classpath `inline` function's anonymous object is regenerated at the call site, and that copy
/// carries the caller's metadata stamp rather than the library class's.
#[test]
fn regenerated_inline_object_takes_the_caller_metadata_stamp() {
    let Some(dir) = common::scratch_dir() else {
        return;
    };
    let lib_out = dir.join("lib");
    std::fs::create_dir_all(&lib_out).unwrap();
    let lib_src = dir.join("Lib.kt");
    std::fs::write(&lib_src, INLINE_LIB).unwrap();
    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        lib_out.to_string_lossy().into_owned(),
        lib_src.to_string_lossy().into_owned(),
    ]) else {
        return;
    };
    assert_eq!(code, 0, "kotlinc lib failed: {stderr}");

    let caller_out = dir.join("caller");
    std::fs::create_dir_all(&caller_out).unwrap();
    let caller_src = dir.join("Caller.kt");
    std::fs::write(&caller_src, INLINE_CALLER).unwrap();
    let stdlib = common::stdlib_jar();
    let classpath = std::env::join_paths([lib_out.as_path(), stdlib.as_path()])
        .expect("caller classpath")
        .to_string_lossy()
        .into_owned();
    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        caller_out.to_string_lossy().into_owned(),
        "-language-version".to_string(),
        "2.2".to_string(),
        "-classpath".to_string(),
        classpath,
        caller_src.to_string_lossy().into_owned(),
    ]) else {
        return;
    };
    assert_eq!(code, 0, "kotlinc caller failed: {stderr}");

    let copy = "CallerKt$box$$inlined$greeter$1";
    let reference = std::fs::read(caller_out.join(format!("{copy}.class")))
        .unwrap_or_else(|_| panic!("kotlinc did not regenerate {copy}"));
    let actual = common::compile_in_process_files_metadata_version(
        &[("Caller.kt", INLINE_CALLER)],
        &[lib_out, stdlib],
        None,
        Some([2, 2, 0]),
    )
    .expect("krusty compiles the caller");
    let (_, actual_bytes) = actual
        .iter()
        .find(|(name, _)| name == copy)
        .unwrap_or_else(|| panic!("krusty did not regenerate {copy}: {actual:?}"));
    assert_eq!(
        metadata_mv(actual_bytes),
        metadata_mv(&reference),
        "the regenerated object stamps the caller's metadata version"
    );
    assert_eq!(metadata_mv(actual_bytes).as_deref(), Some(&[2, 2, 0][..]));
}

/// Every annotation use site a `@Metadata` payload records: class, primary constructor, constructor
/// value parameter, member function and its parameter, property (via its `$annotations` marker),
/// enum entry, and a top-level function with an annotated parameter. The constructor's `val`
/// property stays UNANNOTATED: kotlinc's LV-dependent default target (KT-73255) adds a PROPERTY
/// target to `@Mark val x` at 2.4 (a `getX$annotations` synthetic appears in its d2), which krusty
/// does not implement — its plain `@Mark val x` matches kotlinc's param-only 2.2 behavior only.
const ANNOTATED_SRC: &str = "package app\n\
    \n\
    annotation class Mark\n\
    \n\
    @Mark\n\
    class Annotated @Mark constructor(@Mark n: Int, val x: Int) {\n\
    \x20   @Mark\n\
    \x20   fun method(@Mark p: Int): Int = p\n\
    \n\
    \x20   @Mark\n\
    \x20   val prop: Int = 1\n\
    }\n\
    \n\
    enum class Kind { @Mark A, B }\n\
    \n\
    @Mark\n\
    fun topLevel(@Mark p: Int): Int = p\n";

const ANNOTATED_CLASSES: &[&str] = &["app/Annotated", "app/AnnKt", "app/Kind", "app/Mark"];

/// kotlinc's `LanguageFeature.AnnotationsInMetadata` (since 2.4) gates the annotation RECORDS in
/// `@Metadata`, not the flags: under `-language-version 2.2` the records and their `d2` strings
/// vanish while every `HAS_ANNOTATIONS` bit stays set (measured on kotlinc 2.4.20 — the property
/// keeps its `syntheticMethod` pointer and flags `8711`, only `Property.annotation` goes). Both
/// stamps are byte-compared against kotlinc: at 2.2 a dropped flag bit or a kept record would change
/// `d1`, and the default-language half proves the gate does not leak into the 2.4 output.
#[test]
fn language_version_2_2_omits_annotation_records_like_kotlinc() {
    let dir = common::scratch_dir().expect("allocate metadata-language fixture");
    let src_path = dir.join("Ann.kt");
    std::fs::write(&src_path, ANNOTATED_SRC).unwrap();

    let out = dir.join("ref");
    std::fs::create_dir_all(&out).unwrap();
    let args = vec![
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        "-language-version".to_string(),
        "2.2".to_string(),
        src_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let language_2_2 = krusty::language_settings::LanguageSettings::new(
        krusty::language_version::LanguageVersion::V2_2,
        None,
        &[],
    )
    .expect("2.2 language settings");
    let gated = common::compile_in_process_files_language_settings(
        &[("Ann.kt", ANNOTATED_SRC)],
        &[common::stdlib_jar()],
        None,
        &language_2_2,
        Some([2, 2, 0]),
    )
    .expect("krusty compiles the annotated fixture");
    assert_metadata_bytes_match(&out, &gated, "under language level 2.2");

    // Source semantics own the feature gate. An internal output-stamp override must not re-enable
    // 2.4 annotation records for a 2.2 compilation.
    let old_language_new_stamp = common::compile_in_process_files_language_settings(
        &[("Ann.kt", ANNOTATED_SRC)],
        &[common::stdlib_jar()],
        None,
        &language_2_2,
        Some([2, 4, 0]),
    )
    .expect("krusty compiles the 2.2 fixture with an independent 2.4 stamp");
    assert_metadata_bytes_match(
        &out,
        &old_language_new_stamp,
        "at language 2.2 with an internal 2.4 metadata stamp",
    );

    // The default 2.4 language configuration keeps the records: the gate must not leak into it.
    let default_out = dir.join("ref-default");
    std::fs::create_dir_all(&default_out).unwrap();
    let default_args = vec![
        "-d".to_string(),
        default_out.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) =
        common::kotlinc_compile(&default_args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc (default stamp) failed: {stderr}");

    let default = common::compile_in_process_files_metadata_version(
        &[("Ann.kt", ANNOTATED_SRC)],
        &[common::stdlib_jar()],
        None,
        None,
    )
    .expect("krusty compiles the annotated fixture at the default stamp");
    assert_metadata_bytes_match(&default_out, &default, "at the default stamp");
}

/// Byte-compare `d2` and `d1` of every fixture class against kotlinc's classes in `reference_out`.
fn assert_metadata_bytes_match(
    reference_out: &std::path::Path,
    actual: &[(String, Vec<u8>)],
    context: &str,
) {
    for class in ANNOTATED_CLASSES {
        let reference = std::fs::read(reference_out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, actual_bytes) = actual
            .iter()
            .find(|(name, _)| name == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        let (reference_d1, reference_d2) =
            super::common_core::kotlin_metadata::raw_kotlin_metadata(&reference)
                .unwrap_or_else(|| panic!("{class}: kotlinc's class carries no @Metadata"));
        let (actual_d1, actual_d2) =
            super::common_core::kotlin_metadata::raw_kotlin_metadata(actual_bytes)
                .unwrap_or_else(|| panic!("{class}: krusty's class carries no @Metadata"));
        assert_eq!(
            actual_d2, reference_d2,
            "{class}: d2 string table {context}"
        );
        assert_eq!(actual_d1, reference_d1, "{class}: d1 protobuf {context}");
    }
}
