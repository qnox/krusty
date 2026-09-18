use super::common;

fn javap_path() -> Option<String> {
    // The pooled JavaRunner carries javap in-process; only a JDK home is required.
    let _ = common::java_home();
    Some("pooled".to_string())
}

fn disassemble(_javap: &str, bytes: &[u8], class_file_name: &str, _tag: &str) -> String {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(class_file_name);
    std::fs::write(&class_file, bytes).expect("write continuation class");
    common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable")
}

fn kotlinc_class(source_name: &str, source: &str, class_name: &str) -> Option<Vec<u8>> {
    let root = common::scratch_dir().expect("reference scratch dir");
    let out = root.join("classes");
    std::fs::create_dir_all(&out).expect("reference output dir");
    let source_path = root.join(format!("{source_name}.kt"));
    std::fs::write(&source_path, source).expect("reference source");
    let compiled = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ]);
    let Some((code, stderr)) = compiled else {
        let _ = std::fs::remove_dir_all(root);
        return None;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_name}.class")))
        .unwrap_or_else(|error| panic!("read kotlinc class {class_name}: {error}"));
    let _ = std::fs::remove_dir_all(root);
    Some(bytes)
}

#[test]
fn continuation_emits_debug_and_enclosing_metadata() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
    let classes = common::compile_in_process_files(
        &[("Debug", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(javap) = javap_path() else {
        return;
    };

    // The arms must be consumed, not returned: an `if` whose arms are tail suspend calls needs no
    // continuation at all — kotlinc emits none for it, so its resume lines have no ground truth.
    let source = "package demo\n\
        class Leafs {\n\
        \x20 suspend fun leaf(value: Int): Int = value\n\
        }\n\
        class Chooser(private val leafs: Leafs) {\n\
        \x20 suspend fun choose(flag: Boolean): Int {\n\
        \x20\x20 val base = 1\n\
        \x20\x20 val picked = if (flag) {\n\
        \x20\x20\x20 leafs.leaf(1)\n\
        \x20\x20 } else {\n\
        \x20\x20\x20 leafs.leaf(2)\n\
        \x20\x20 }\n\
        \x20\x20 return picked + base\n\
        \x20 }\n\
        }\n";
    let Some(reference) = kotlinc_class("ReturnedExpression", source, "demo/Chooser$choose$1")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classes = common::compile_in_process_files(
        &[("ReturnedExpression", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile returned expression continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Chooser$choose$1").then_some(bytes))
        .expect("choose continuation");
    let text = disassemble(
        &javap,
        bytes,
        "Chooser$choose$1.class",
        "returned_expression",
    );
    let reference_text = disassemble(
        &javap,
        &reference,
        "ref-Chooser$choose$1.class",
        "returned_expression_reference",
    );
    let lines = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| line.starts_with("l=[") || line.starts_with("nl=["))
            .map(str::to_string)
            .collect()
    };

    assert_eq!(
        lines(&text),
        lines(&reference_text),
        "suspension and resume lines must match kotlinc\nkrusty:\n{text}\nkotlinc:\n{reference_text}"
    );
}

#[test]
fn continuation_metadata_keeps_names_with_each_scope_snapshot() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
fn continuation_metadata_omits_unnamed_loop_spills() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun consume(value: String): String = value\n\
        suspend fun work(values: List<String>, seed: String): String {\n\
        \x20 var result = seed\n\
        \x20 for (value in values) {\n\
        \x20\x20 consume(value)\n\
        \x20\x20 result += value\n\
        \x20 }\n\
        \x20 return result\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("LoopSpills", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile loop-spill continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/LoopSpillsKt$work$1").then_some(bytes))
        .expect("work continuation");
    let text = disassemble(&javap, bytes, "LoopSpillsKt$work$1.class", "loop_spills");
    let annotation = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .map(|(_, annotation)| annotation)
        .expect("runtime-visible annotations");
    let names = annotation
        .lines()
        .find(|line| line.trim_start().starts_with("n=["))
        .expect("debug metadata names");
    let spill_fields: Vec<_> = text
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("java.lang.Object ")
                .and_then(|field| field.strip_suffix(';'))
                .filter(|field| field.starts_with("L$"))
        })
        .collect();

    assert!(!names.contains("\"\""), "{text}");
    assert!(
        spill_fields
            .iter()
            .any(|field| !annotation.contains(&format!("\"{field}\""))),
        "{text}"
    );
}

#[test]
fn continuation_metadata_uses_later_value_branch_as_resume_line() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
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
        Some(jdk.as_path()),
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
    let stdlib = common::stdlib_jar();
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

/// A `for` loop around a suspension spills FOUR values: the two locals in scope, the loop's iterator,
/// and the loop variable. kotlinc numbers the `L$N` fields in the order the values are DECLARED — the
/// iterator, created when the loop opens, takes `L$2` and the loop variable `L$3` — and the `s` array
/// names the field each source variable landed in, skipping the iterator, which has no name.
///
/// krusty numbered them by IR index instead, where a compiler temporary sorts after every named
/// local, so the iterator took the LAST field and the loop variable claimed `L$2`. Every suspending
/// loop in the corpus carries that difference.
#[test]
fn continuation_metadata_numbers_spills_in_declaration_order() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun find(name: String): String? = name\n\
        suspend fun load(names: List<String>): List<String> {\n\
        \x20 val out = ArrayList<String>()\n\
        \x20 for (name in names) {\n\
        \x20   val found = find(name)\n\
        \x20   if (found != null) out.add(found)\n\
        \x20 }\n\
        \x20 return out\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("SpillOrder", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile the suspending loop");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/SpillOrderKt$load$1").then_some(bytes))
        .expect("load continuation");
    let text = disassemble(&javap, bytes, "SpillOrderKt$load$1.class", "spill_order");

    let Some(reference_bytes) = kotlinc_class("SpillOrder", source, "demo/SpillOrderKt$load$1")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let reference_text = disassemble(
        &javap,
        &reference_bytes,
        "ReferenceSpillOrderKt$load$1.class",
        "reference_spill_order",
    );

    let debug_metadata = |output: &str| {
        let mut lines = output
            .lines()
            .skip_while(|line| !line.contains("kotlin.coroutines.jvm.internal.DebugMetadata("));
        let first = lines.next().expect("DebugMetadata annotation");
        let mut block = vec![first.trim().to_string()];
        for line in lines {
            let line = line.trim().to_string();
            let end = line == ")";
            block.push(line);
            if end {
                return block;
            }
        }
        panic!("unterminated DebugMetadata annotation:\n{output}")
    };
    assert_eq!(
        debug_metadata(&text),
        debug_metadata(&reference_text),
        "continuation DebugMetadata must exactly match kotlinc\nkrusty:\n{text}\nkotlinc:\n{reference_text}"
    );
}

/// A continuation's constant pool follows kotlinc's VISIT order: the spill fields' names and their
/// one shared descriptor intern with the field table, `@DebugMetadata` after them — its `s` array
/// names those very fields — and `result`/`this$0`/`label` only where they are first USED, in the
/// constructor and `invokeSuspend`.
///
/// krusty built the annotation before the fields, so the subset of `L$N` names the array mentions
/// interned ahead of the table, and it declared all three remaining fields eagerly, which pushed the
/// constructor's own strings down. Neither changes what the class says, and both shift every entry
/// after them — a class that matches kotlinc member for member still differs byte for byte.
#[test]
fn continuation_pool_interns_spills_then_metadata_then_used_fields() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        class Loader {\n\
        \x20 suspend fun find(name: String): String? = name\n\
        \x20 suspend fun load(names: List<String>): List<String> {\n\
        \x20   val out = ArrayList<String>()\n\
        \x20   for (name in names) {\n\
        \x20     val found = find(name)\n\
        \x20     if (found != null) out.add(found)\n\
        \x20   }\n\
        \x20   return out\n\
        \x20 }\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("PoolOrder", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile the suspending loop");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Loader$load$1").then_some(bytes))
        .expect("load continuation");
    let text = disassemble(&javap, bytes, "Loader$load$1.class", "pool_order");
    let Some(reference_bytes) = kotlinc_class("PoolOrder", source, "demo/Loader$load$1") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let reference_text = disassemble(
        &javap,
        &reference_bytes,
        "ReferenceLoader$load$1.class",
        "reference_pool_order",
    );
    let visit_order = |output: &str| {
        const CONTRACT: [&str; 9] = [
            "L$0",
            "L$1",
            "L$2",
            "L$3",
            "Lkotlin/coroutines/jvm/internal/DebugMetadata;",
            "<init>",
            "this$0",
            "result",
            "label",
        ];
        output
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('#') && line.contains(" = "))
            .filter_map(|line| line.split_once(" = ").map(|(_, entry)| entry))
            .filter_map(|entry| entry.split_whitespace().nth(1))
            .filter(|value| CONTRACT.contains(value))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let reference_order = visit_order(&reference_text);
    assert_eq!(
        reference_order,
        [
            "L$0",
            "L$1",
            "L$2",
            "L$3",
            "Lkotlin/coroutines/jvm/internal/DebugMetadata;",
            "<init>",
            "this$0",
            "result",
            "label",
        ],
        "fixture must exercise kotlinc's complete continuation visit-order contract:\n{reference_text}"
    );
    assert_eq!(
        visit_order(&text),
        reference_order,
        "continuation constant-pool visit order must exactly match kotlinc\nkrusty:\n{text}\nkotlinc:\n{reference_text}"
    );
}

/// The whole continuation class, byte for byte. Everything the other tests in this file assert
/// separately — the `@DebugMetadata` arrays, the spill positions, the pool's visit order, the
/// constructor's header and its `LocalVariableTable` — has to hold at once for this to pass, which is
/// what makes it the real check: a suspending function's continuation is the single commonest
/// generated class in a coroutine-using corpus.
#[test]
fn a_suspending_loops_continuation_is_byte_identical() {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: no scratch dir");
        return;
    };
    let source = "package demo\n\
        class Store {\n\
        \x20 suspend fun find(name: String): String? = name\n\
        }\n\
        class Loader(private val store: Store) {\n\
        \x20 suspend fun load(names: List<String>): List<String> {\n\
        \x20   val out = ArrayList<String>()\n\
        \x20   for (name in names) {\n\
        \x20     val found = store.find(name)\n\
        \x20     if (found != null) out.add(found)\n\
        \x20   }\n\
        \x20   return out\n\
        \x20 }\n\
        }\n";
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    let file = dir.join("Identical.kt");
    std::fs::write(&file, source).expect("write fixture");
    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        file.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp_module_target(
        source,
        "Identical",
        &[common::stdlib_jar(), common::jdk_modules()],
        "main",
        Some(69),
    )
    .expect("krusty compiles the fixture");
    let continuation = "demo/Loader$load$1";
    let ours = classes
        .iter()
        .find_map(|(name, bytes)| (name == continuation).then_some(bytes))
        .expect("load continuation");
    let theirs = std::fs::read(reference_dir.join(format!("{continuation}.class")))
        .expect("reference continuation");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        ours.len(),
        theirs.len(),
        "{continuation}: {} bytes vs kotlinc's {}",
        ours.len(),
        theirs.len()
    );
    assert!(
        ours.as_slice() == theirs.as_slice(),
        "{continuation} differs from kotlinc at byte {}",
        ours.iter()
            .zip(&theirs)
            .position(|(a, b)| a != b)
            .unwrap_or(0)
    );
}

/// `i`, `s` and `n` are one zipped debugger contract: `i[k]` says which resume state `s[k]`/`n[k]`
/// belong to. A single-suspension fixture cannot catch a misalignment between them, because every
/// `i` entry is zero and the arrays cannot disagree about state boundaries.
///
/// Two suspensions with different live sets, and both kinds of non-reference spill, so the state
/// concatenation and the per-state reset of the positions are both observable. Compared against
/// kotlinc rather than pinned, so the contract stays tied to the reference compiler.
#[test]
fn continuation_metadata_zips_its_arrays_across_two_states() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(javap) = javap_path() else {
        return;
    };

    let source = "package demo\n\
        suspend fun leaf(): String = \"\"\n\
        suspend fun work(r: String, a: Int, b: Long): String {\n\
        \x20 val first = a + 1\n\
        \x20 val x = leaf()\n\
        \x20 val before = r + x\n\
        \x20 val between = b + 1\n\
        \x20 val y = leaf()\n\
        \x20 return before + between + first + y\n\
        }\n";
    let Some(reference) = kotlinc_class("TwoStates", source, "demo/TwoStatesKt$work$1") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classes = common::compile_in_process_files(
        &[("TwoStates", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile two-state continuation");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/TwoStatesKt$work$1").then_some(bytes))
        .expect("work continuation");

    let arrays = |text: &str| -> Vec<String> {
        let annotation = text
            .rsplit_once("RuntimeVisibleAnnotations:")
            .map(|(_, annotation)| annotation)
            .expect("runtime-visible annotations");
        annotation
            .split_whitespace()
            .filter(|token| {
                token.starts_with("i=[") || token.starts_with("s=[") || token.starts_with("n=[")
            })
            .map(str::to_string)
            .collect()
    };
    let want = arrays(&disassemble(
        &javap,
        &reference,
        "TwoStatesKt$work$1.class",
        "two_states_reference",
    ));
    assert_eq!(
        want,
        [
            "i=[0,0,0,0,1,1,1,1,1,1,1]",
            "s=[\"L$0\",\"I$0\",\"J$0\",\"I$1\",\"L$0\",\"L$1\",\"L$2\",\"I$0\",\"J$0\",\"I$1\",\"J$1\"]",
            "n=[\"r\",\"a\",\"b\",\"first\",\"r\",\"x\",\"before\",\"a\",\"b\",\"first\",\"between\"]",
        ],
        "kotlinc's own arrays, spelled out so a reference change is visible here"
    );
    assert_eq!(
        arrays(&disassemble(
            &javap,
            bytes,
            "TwoStatesKt$work$1.class",
            "two_states"
        )),
        want,
        "complete zipped DebugMetadata arrays"
    );
}
