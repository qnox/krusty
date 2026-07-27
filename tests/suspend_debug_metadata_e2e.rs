use std::process::Command;

use super::common;

#[test]
fn continuation_emits_runtime_visible_debug_metadata() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())
    else {
        return;
    };
    let javap = format!("{java_home}/bin/javap");
    if !std::path::Path::new(&javap).exists() {
        return;
    }

    let source = "package demo\n\
        suspend fun leaf(value: Int): Int = value\n\
        suspend fun work(value: Int): Int {\n\
        \x20 val saved = value\n\
        \x20 val resumed = leaf(value)\n\
        \x20 return saved + resumed\n\
        }\n";
    let classes =
        common::compile_in_process_files(&[("Debug", source)], &[stdlib, jdk.clone()], Some(&jdk))
            .expect("compile suspend continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/DebugKt$work$1").then_some(bytes))
        .expect("work continuation");

    let dir = std::env::temp_dir().join(format!(
        "krusty_suspend_debug_metadata_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create javap directory");
    let class_file = dir.join("DebugKt$work$1.class");
    std::fs::write(&class_file, bytes).expect("write continuation class");
    let output = Command::new(javap)
        .args(["-v", "-p"])
        .arg(&class_file)
        .output()
        .expect("run javap");
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        output.status.success(),
        "javap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");
    assert!(
        annotation.contains("kotlin.coroutines.jvm.internal.DebugMetadata("),
        "{text}"
    );
    for expected in [
        "f=\"Debug.kt\"",
        "l=[5]",
        "nl=[6]",
        "i=[0,0]",
        "s=[\"I$0\",\"I$1\"]",
        "n=[\"value\",\"saved\"]",
        "m=\"work\"",
        "c=\"demo.DebugKt\"",
        "v=2",
    ] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}
