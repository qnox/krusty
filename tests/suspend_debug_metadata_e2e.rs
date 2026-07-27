use std::process::Command;

use super::common;

fn javap_path() -> Option<String> {
    let java_home = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())?;
    let javap = format!("{java_home}/bin/javap");
    std::path::Path::new(&javap).exists().then_some(javap)
}

fn disassemble(javap: &str, bytes: &[u8], class_file_name: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "krusty_suspend_metadata_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create javap directory");
    let class_file = dir.join(class_file_name);
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
    String::from_utf8(output.stdout).expect("javap output")
}

#[test]
fn continuation_emits_debug_and_enclosing_metadata() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

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

    let text = disassemble(&javap, bytes, "DebugKt$work$1.class", "debug");
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

    let enclosing = text
        .lines()
        .find(|line| line.trim_start().starts_with("EnclosingMethod:"))
        .expect("EnclosingMethod attribute");
    assert!(
        enclosing.contains("// demo.DebugKt.work"),
        "wrong enclosing method:\n{text}"
    );
    let name_and_type = enclosing
        .split_once('.')
        .and_then(|(_, suffix)| suffix.split_whitespace().next())
        .expect("EnclosingMethod name-and-type index");
    let prefix = format!("{name_and_type} = NameAndType");
    let descriptor = text
        .lines()
        .find(|line| line.trim_start().starts_with(&prefix))
        .expect("EnclosingMethod name-and-type entry");
    assert!(
        descriptor.contains("work:(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;"),
        "wrong enclosing descriptor:\n{text}"
    );
}

#[test]
fn continuation_metadata_repeats_spills_for_each_suspension() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf(value: String) {}\n\
        suspend fun work(orgId: String, id: String): String {\n\
        \x20 leaf(orgId)\n\
        \x20 leaf(id)\n\
        \x20 return orgId + id\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("MultiState", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile multi-state continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/MultiStateKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(&javap, bytes, "MultiStateKt$work$1.class", "multi_state");
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in [
        "i=[0,0,1,1]",
        "s=[\"L$0\",\"L$1\",\"L$0\",\"L$1\"]",
        "n=[\"orgId\",\"id\",\"orgId\",\"id\"]",
    ] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_preserves_elvis_subject_lines() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun find(id: String): String? = id\n\
        suspend fun work(id: String): String {\n\
        \x20 val value = find(id) ?: return \"missing\"\n\
        \x20 return value\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("ElvisLine", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile elvis continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/ElvisLineKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(&javap, bytes, "ElvisLineKt$work$1.class", "elvis_line");
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[4]", "nl=[5]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_orders_mixed_spills_and_terminal_resume() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf() {}\n\
        suspend fun work(count: Int, ref: String) {\n\
        \x20 leaf()\n\
        \x20 leaf()\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("MixedSpills", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile mixed-spill continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/MixedSpillsKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(&javap, bytes, "MixedSpillsKt$work$1.class", "mixed_spills");
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in [
        "l=[4,5]",
        "nl=[5,6]",
        "i=[0,0,1,1]",
        "s=[\"L$0\",\"I$0\",\"L$0\",\"I$0\"]",
        "n=[\"ref\",\"count\",\"ref\",\"count\"]",
    ] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_multiline_call_selector_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        class Repo {\n\
        \x20 suspend fun find(): String = \"\"\n\
        }\n\
        suspend fun work(repo: Repo): String {\n\
        \x20 val value = repo\n\
        \x20\x20 .find()\n\
        \x20 return value\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("SelectorLine", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile multiline selector continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/SelectorLineKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "SelectorLineKt$work$1.class",
        "selector_line",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[7]", "nl=[6]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_nested_branch_resume_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf(): Int = 1\n\
        suspend fun work(flag: Boolean): Int {\n\
        \x20 if (flag) {\n\
        \x20\x20 val value = leaf()\n\
        \x20\x20 return value\n\
        \x20 }\n\
        \x20 return 0\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("NestedResume", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile nested branch continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/NestedResumeKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "NestedResumeKt$work$1.class",
        "nested_resume",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[5]", "nl=[6]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_returned_expression_end_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf(value: Int): Int = value\n\
        suspend fun choose(flag: Boolean): Int {\n\
        \x20 return if (flag) {\n\
        \x20\x20 leaf(1)\n\
        \x20 } else {\n\
        \x20\x20 leaf(2)\n\
        \x20 }\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("ReturnedExpression", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile returned expression continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/ReturnedExpressionKt$choose$1").then_some(bytes))
        .expect("choose continuation");
    let text = disassemble(
        &javap,
        bytes,
        "ReturnedExpressionKt$choose$1.class",
        "returned_expression",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[5,7]", "nl=[8,8]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_keeps_names_with_each_scope_snapshot() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf(value: String): String = value\n\
        suspend fun work(input: String): String {\n\
        \x20 val first = leaf(input)\n\
        \x20 val alias = first\n\
        \x20 val second = leaf(alias)\n\
        \x20 return input + second\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("ScopeNames", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile scope-name continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/ScopeNamesKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(&javap, bytes, "ScopeNamesKt$work$1.class", "scope_names");
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in [
        "i=[0,1,1,1]",
        "s=[\"L$0\",\"L$0\",\"L$1\",\"L$2\"]",
        "n=[\"input\",\"input\",\"first\",\"alias\"]",
    ] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_later_value_branch_as_resume_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun find(): String? = null\n\
        suspend fun work(): String {\n\
        \x20 val value =\n\
        \x20\x20 find()\n\
        \x20\x20\x20 ?: error(\"missing\")\n\
        \x20 return value\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("ValueBranchLine", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile value-branch continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/ValueBranchLineKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "ValueBranchLineKt$work$1.class",
        "value_branch_line",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[5]", "nl=[6]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_try_end_and_catch_name() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf() {}\n\
        suspend fun work() {\n\
        \x20 try {\n\
        \x20\x20 leaf()\n\
        \x20 } catch (failure: Exception) {\n\
        \x20\x20 leaf()\n\
        \x20\x20 println(failure.message)\n\
        \x20 }\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("TryResumeLine", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile try continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/TryResumeLineKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "TryResumeLineKt$work$1.class",
        "try_resume_line",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in [
        "l=[5,7]",
        "nl=[6,8]",
        "i=[1]",
        "s=[\"L$0\"]",
        "n=[\"failure\"]",
    ] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_successor_initializer_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf() {}\n\
        suspend fun work(flag: Boolean): Int {\n\
        \x20 leaf()\n\
        \x20 val value =\n\
        \x20\x20 when (flag) { true -> 1; else -> 2 }\n\
        \x20 return value\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("SuccessorLine", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile successor-line continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/SuccessorLineKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "SuccessorLineKt$work$1.class",
        "successor_line",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[4]", "nl=[6]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_marks_direct_tail_suspension() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf(value: Int): Int = value\n\
        suspend fun work(): Int {\n\
        \x20 val first = leaf(1)\n\
        \x20 return leaf(first)\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("TailResume", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile tail-resume continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/TailResumeKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(&javap, bytes, "TailResumeKt$work$1.class", "tail_resume");
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[4,5]", "nl=[5,-1]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_inline_lambda_smap_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun values(): List<Int> = listOf(1)\n\
        suspend fun work(): List<Int> =\n\
        \x20 values().filter { it > 0 }\n";
    let classes = common::compile_in_process_files(
        &[("InlineResume", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile inline-resume continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/InlineResumeKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "InlineResumeKt$work$1.class",
        "inline_resume",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[4]", "nl=[6]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_trailing_lambda_selector_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun values(): List<Int> = listOf(1)\n\
        suspend fun work(): List<Int> {\n\
        \x20 val result = values()\n\
        \x20\x20 .filter { it > 0 }\n\
        \x20 return result\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("TrailingLambdaLine", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile trailing-lambda continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/TrailingLambdaLineKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "TrailingLambdaLineKt$work$1.class",
        "trailing_lambda_line",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[4]", "nl=[5]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_metadata_uses_named_member_call_selector_line() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        class Recorder {\n\
        \x20 suspend fun save(label: String, value: Int): Int = value\n\
        }\n\
        suspend fun work(recorder: Recorder): Int {\n\
        \x20 val result = recorder.save(\n\
        \x20\x20 value = 3,\n\
        \x20\x20 label = \"entry\",\n\
        \x20 )\n\
        \x20 return result\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("NamedMemberLine", source)],
        &[stdlib, jdk.clone()],
        Some(&jdk),
    )
    .expect("compile named member call continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/NamedMemberLineKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(
        &javap,
        bytes,
        "NamedMemberLineKt$work$1.class",
        "named_member_line",
    );
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");

    for expected in ["l=[6]", "nl=[10]"] {
        assert!(
            annotation.contains(expected),
            "missing {expected:?}:\n{text}"
        );
    }
}

#[test]
fn continuation_uses_synthetic_kotlin_metadata() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(javap) = javap_path() else {
        return;
    };
    let source = "package demo\n\
        class Service {\n\
        \x20 suspend fun leaf(value: Int): Int = value\n\
        \x20 suspend fun work(value: Int): Int {\n\
        \x20\x20 val saved = value\n\
        \x20\x20 return saved + leaf(value)\n\
        \x20 }\n\
        }\n";
    let classes = common::compile_in_process_metadata_cp(source, "Synthetic", &[stdlib])
        .expect("compile suspend continuation metadata");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Service$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(&javap, bytes, "Service$work$1.class", "synthetic");

    let metadata = text
        .rsplit_once("kotlin.Metadata(")
        .map(|(_, metadata)| metadata)
        .expect("Kotlin metadata");
    for expected in ["mv=[2,4,0]", "k=3", "xi=48"] {
        assert!(metadata.contains(expected), "missing {expected:?}:\n{text}");
    }
    for forbidden in [
        "d1=[",
        "d2=[",
        "getResult",
        "setResult",
        "getLabel",
        "setLabel",
        "getI$0",
        "setI$0",
    ] {
        assert!(
            !text.contains(forbidden),
            "unexpected {forbidden:?}:\n{text}"
        );
    }

    let constructor = text
        .split_once("demo.Service$work$1(demo.Service, kotlin.coroutines.Continuation<")
        .map(|(_, section)| section)
        .unwrap_or_else(|| panic!("continuation constructor:\n{text}"));
    let (constructor, invoke_suspend) = constructor
        .split_once("public final java.lang.Object invokeSuspend")
        .expect("invokeSuspend method");
    for expected in [
        "Signature:",
        "(Ldemo/Service;Lkotlin/coroutines/Continuation<-Ldemo/Service$work$1;>;)V",
        "LocalVariableTable:",
        "this$0",
        "$completion",
    ] {
        assert!(
            constructor.contains(expected),
            "missing constructor metadata {expected:?}:\n{text}"
        );
    }
    for expected in ["LocalVariableTable:", "$result"] {
        assert!(
            invoke_suspend.contains(expected),
            "missing invokeSuspend metadata {expected:?}:\n{text}"
        );
    }
    let (invoke_body, invoke_parameter_annotations) = invoke_suspend
        .split_once("RuntimeInvisibleParameterAnnotations:")
        .expect("invokeSuspend parameter annotations");
    assert!(
        invoke_body.contains("org.jetbrains.annotations.Nullable"),
        "invokeSuspend return must be nullable:\n{text}"
    );
    assert!(
        invoke_parameter_annotations.contains("org.jetbrains.annotations.NotNull"),
        "invokeSuspend parameter must be non-null:\n{text}"
    );
    assert!(
        !constructor.contains("LineNumberTable:") && !invoke_suspend.contains("LineNumberTable:"),
        "continuation methods must not carry line tables:\n{text}"
    );
    let receiver_store = constructor
        .find("// Field this$0:Ldemo/Service;")
        .expect("receiver field store");
    let super_call = constructor
        .find("// Method kotlin/coroutines/jvm/internal/ContinuationImpl")
        .expect("continuation superclass constructor");
    assert!(
        receiver_store < super_call,
        "receiver must be stored before the superclass constructor call:\n{text}"
    );
    let fields = text
        .split_once('{')
        .and_then(|(_, fields)| fields.split_once("demo.Service$work$1("))
        .map(|(fields, _)| fields)
        .expect("continuation fields");
    assert!(
        !fields.contains("RuntimeInvisibleAnnotations:"),
        "continuation fields must not carry property annotations:\n{text}"
    );
    let field_positions = ["I$0", "I$1", "result", "this$0", "label"].map(|field| {
        fields
            .find(&format!(" {field};"))
            .unwrap_or_else(|| panic!("missing field {field:?}:\n{text}"))
    });
    assert!(
        field_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "wrong continuation field order:\n{text}"
    );
    assert!(
        !text.contains("of class demo/Service$work"),
        "continuation must be an anonymous InnerClasses entry:\n{text}"
    );
    assert!(
        text.contains("InnerClasses:\n  static final"),
        "continuation must be a static final InnerClasses entry:\n{text}"
    );
    assert!(
        !text.lines().any(|line| line.trim_start().starts_with("#")
            && line.contains("Utf8")
            && line.ends_with(" 1")),
        "anonymous continuation name must not be interned:\n{text}"
    );
}
