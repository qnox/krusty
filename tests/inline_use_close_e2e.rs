//! Inlined generic `use`: erased close, generic `Unit`, same-file SMAP, sorted handlers.
//!
//! `T.use` stores its receiver as the erased bound. A method-return receiver is not known
//! non-null after the checkcast, so each `this?.close()` is `dup; ifnull`. A discarded `R = Unit`
//! still stores `kotlin.Unit.INSTANCE` before `finally` and does not reload it. The same file's
//! expansion writes a source map, and only the method that actually inlined sorts its exception
//! table.

use std::path::{Path, PathBuf};

use super::common;

const DISCARDED: &str = r#"import java.io.BufferedReader
import java.io.StringReader

class Holder {
    fun bufferedReader(): BufferedReader = BufferedReader(StringReader(""))
}

inline fun <T : AutoCloseable?, R> T.use(block: (T) -> R): R {
    var closed = false
    try {
        return block(this)
    } catch (e: Exception) {
        closed = true
        try {
            this?.close()
        } catch (closeException: Exception) {
        }
        throw e
    } finally {
        if (!closed) {
            this?.close()
        }
    }
}

fun load(holder: Holder): Int {
    var n = 0
    holder.bufferedReader().use { r ->
        n += r.read()
    }
    return n
}

fun box(): String {
    val n = load(Holder())
    return if (n == -1) "OK" else "F:$n"
}
"#;

const USED: &str = r#"import java.io.BufferedReader
import java.io.StringReader

class Holder {
    fun bufferedReader(): BufferedReader = BufferedReader(StringReader("ab"))
}

inline fun <T : AutoCloseable?, R> T.use(block: (T) -> R): R {
    var closed = false
    try {
        return block(this)
    } catch (e: Exception) {
        closed = true
        try {
            this?.close()
        } catch (closeException: Exception) {
        }
        throw e
    } finally {
        if (!closed) {
            this?.close()
        }
    }
}

fun load(holder: Holder): Int {
    val n = holder.bufferedReader().use { r -> r.read() }
    return n
}

fun box(): String {
    val n = load(Holder())
    return if (n == 'a'.code) "OK" else "F:$n"
}
"#;

fn compile_krusty(tag: &str, source: &str) -> Vec<(String, Vec<u8>)> {
    use krusty::diag::DiagSink;
    use krusty::jvm::JvmBackend;
    use krusty::source::SourceInput;

    let mut diags = DiagSink::new();
    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()).expect("JVM provider"),
    );
    let inputs = [SourceInput::kotlin(source).with_file_stem("UseClose")];
    let stems = ["UseClose".to_string()];
    let features = krusty::features::LangFeatures::from_source(source);
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        common::with_native_plugins(platform),
        &features,
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );
    let outputs = krusty::compiler::emit_analyzed(
        analysis,
        &stems,
        &JvmBackend::new(cp),
        "main",
        &mut diags,
    );
    let classes = outputs
        .into_iter()
        .filter_map(|(path, bytes)| {
            path.strip_suffix(".class")
                .map(|internal| (internal.to_string(), bytes))
        })
        .collect::<Vec<_>>();
    if diags.has_errors() || classes.is_empty() {
        panic!(
            "{tag}: krusty failed to compile\n{}",
            diags.render_all(&[("UseClose.kt", source)])
        );
    }
    classes
}

fn classes(tag: &str, source: &str) -> (PathBuf, PathBuf) {
    let root = common::scratch_dir().unwrap_or_else(|| panic!("{tag}: scratch directory"));
    let reference = root.join("kotlinc");
    let krusty = root.join("krusty");
    std::fs::create_dir_all(&reference).unwrap();
    std::fs::create_dir_all(&krusty).unwrap();
    let file = root.join("UseClose.kt");
    std::fs::write(&file, source).unwrap();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        file.to_string_lossy().into_owned(),
    ])
    .unwrap_or_else(|| panic!("{tag}: kotlinc unavailable"));
    assert_eq!(code, 0, "{tag}: kotlinc failed: {stderr}");
    let emitted = compile_krusty(tag, source);
    for (name, bytes) in emitted {
        let path = krusty.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }
    (reference, krusty)
}

fn javap_class(root: &Path) -> String {
    common::javap(&[
        "-c",
        "-p",
        "-v",
        "-l",
        "-cp",
        &root.to_string_lossy(),
        "UseCloseKt",
    ])
    .unwrap_or_else(|| panic!("javap failed for {}", root.display()))
}

/// Bytecode of one method, from its `flags:` line through the line-number table.
fn method_body(dump: &str, signature: &str) -> String {
    let mut lines = dump.lines();
    let mut body = Vec::new();
    let mut started = false;
    for line in lines.by_ref() {
        if !started {
            if line.contains(signature) {
                started = true;
            }
            continue;
        }
        if line.starts_with("  public ") || line.starts_with("}") || line.starts_with("SourceFile:")
        {
            break;
        }
        body.push(line);
    }
    body.join("\n")
}

fn section(body: &str, header: &str, next: &[&str]) -> String {
    let mut out = Vec::new();
    let mut started = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if !started {
            if trimmed.starts_with(header) {
                started = true;
            }
            continue;
        }
        if next.iter().any(|stop| trimmed.starts_with(stop)) {
            break;
        }
        if trimmed.is_empty() {
            continue;
        }
        out.push(normalize_insn(trimmed));
    }
    out.join("\n")
}

fn normalize_insn(line: &str) -> String {
    let code = line.split("//").next().unwrap_or(line).trim();
    let mut parts = code.splitn(2, ':');
    let head = parts.next().unwrap_or("");
    if head.chars().all(|ch| ch.is_ascii_digit()) {
        parts.next().unwrap_or("").trim().to_string()
    } else {
        code.to_string()
    }
}

fn assert_method_matches(tag: &str, reference: &str, krusty: &str, signature: &str) {
    let reference = method_body(reference, signature);
    let krusty = method_body(krusty, signature);
    let code_stops = ["Exception table:", "StackMapTable:", "LineNumberTable:"];
    let reference_code = section(&reference, "Code:", &code_stops);
    let krusty_code = section(&krusty, "Code:", &code_stops);
    assert_eq!(krusty_code, reference_code, "{tag}: {signature} instructions");
    let table_stops = ["StackMapTable:", "LineNumberTable:", "LocalVariableTable:"];
    assert_eq!(
        section(&krusty, "Exception table:", &table_stops),
        section(&reference, "Exception table:", &table_stops),
        "{tag}: {signature} exception table"
    );
    assert_eq!(
        section(&krusty, "LineNumberTable:", &["LocalVariableTable:", "Runtime"]),
        section(&reference, "LineNumberTable:", &["LocalVariableTable:", "Runtime"]),
        "{tag}: {signature} lines"
    );
}

#[test]
fn discarded_generic_use_matches_kotlinc() {
    let (reference_dir, krusty_dir) = classes("discarded", DISCARDED);
    let reference = javap_class(&reference_dir);
    let krusty = javap_class(&krusty_dir);
    assert_method_matches("discarded", &reference, &krusty, "int load(Holder)");
    assert_method_matches(
        "discarded",
        &reference,
        &krusty,
        "R use(T, kotlin.jvm.functions.Function1",
    );
    assert_eq!(
        common::source_debug_extension(&reference_dir.join("UseCloseKt.class")),
        common::source_debug_extension(&krusty_dir.join("UseCloseKt.class")),
        "same-file use expansion writes kotlinc's source map"
    );
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let result = common::run_box(&[], "UseCloseKt", &[krusty_dir, stdlib, jdk])
        .expect("run discarded use");
    assert_eq!(result, "OK");
}

#[test]
fn used_generic_use_stores_the_primitive_result() {
    let (reference_dir, krusty_dir) = classes("used", USED);
    let reference = javap_class(&reference_dir);
    let krusty = javap_class(&krusty_dir);
    assert_method_matches("used", &reference, &krusty, "int load(Holder)");
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let result =
        common::run_box(&[], "UseCloseKt", &[krusty_dir, stdlib, jdk]).expect("run used use");
    assert_eq!(result, "OK");
}
