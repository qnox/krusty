//! A CLASSPATH annotation on an enum constant is resolved through the file's imports and emitted with
//! the retention its compiled annotation class declares: `@Retention(RUNTIME)` →
//! `RuntimeVisibleAnnotations`, `@Retention(BINARY)` → `RuntimeInvisibleAnnotations` — matching
//! kotlinc. (This is the `@SerialName` case: the annotation type lives in another module.)

use super::common;

const LIB: &str = "package lib\n\
    @Retention(AnnotationRetention.RUNTIME)\n\
    annotation class Vis(val v: String)\n\
    @Retention(AnnotationRetention.BINARY)\n\
    annotation class Inv(val v: String)\n\
    @Retention(AnnotationRetention.SOURCE)\n\
    annotation class Src(val v: String)\n";

fn role_bytes() -> Vec<u8> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let lib = common::compile_lib_ref("annlib", LIB).expect("compile annotation lib");
    let classes = common::compile_in_process(
        "package demo\n\
         import lib.Vis\n\
         import lib.Inv\n\
         import lib.Src\n\
         enum class Role {\n\
             @Vis(\"a\") @Inv(\"b\") @Src(\"c\") A,\n\
             B,\n\
         }\n",
        "File",
        &[lib, sl, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile enum against the annotation lib");
    classes
        .into_iter()
        .find(|(n, _)| n == "demo/Role")
        .expect("demo/Role emitted")
        .1
}

fn contains(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn classpath_annotation_retention_splits_visible_invisible_drops_source() {
    let bytes = role_bytes();
    assert!(
        contains(&bytes, "RuntimeVisibleAnnotations"),
        "no visible attr"
    );
    assert!(contains(&bytes, "Llib/Vis;"), "RUNTIME annotation missing");
    assert!(
        contains(&bytes, "RuntimeInvisibleAnnotations"),
        "no invisible attr"
    );
    assert!(contains(&bytes, "Llib/Inv;"), "BINARY annotation missing");
    assert!(
        !contains(&bytes, "Llib/Src;"),
        "SOURCE annotation must be dropped"
    );
}

#[test]
fn classpath_low_priority_annotation_reaches_overload_selection() {
    let library = common::compile_lib_ref(
        "low_priority_metadata",
        r#"package lib
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
@kotlin.internal.LowPriorityInOverloadResolution
fun choose(value: Int) = "low"
fun choose(value: Any) = "normal"
"#,
    )
    .expect("compile low-priority library");
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classes = common::compile_in_process(
        "import lib.choose\nfun box(): String = choose(1)\n",
        "LowPriorityConsumer",
        &[library.clone(), stdlib.clone(), jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile consumer");
    let output = common::run_box(&classes, "LowPriorityConsumerKt", &[library, stdlib])
        .expect("pooled box runner unavailable");
    assert_eq!(output.trim(), "normal");
}
