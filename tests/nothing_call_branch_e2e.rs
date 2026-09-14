//! A `Nothing`-returning function CALL (not `throw`/`return`) used as a branch of an `if`/`when`
//! statement must terminate that path — kotlinc discards the physical `Void` the call leaves and throws
//! `KotlinNothingValueException`. Without that, the diverging branch leaks a `Void` into the merge frame
//! (VerifyError: inconsistent stackmap frames). Round-tripped on the JVM.

use super::common;
use std::fs;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn run_both(src: &str, what: &str) {
    common::expect_box_ok_with_stdlib(src, "Main");
    let out = common::kotlinc_library(src)
        .unwrap_or_else(|| panic!("{what}: reference compiler unavailable"));
    let actual = common::run_box(&[], "LibKt", &[out, common::stdlib_jar()])
        .unwrap_or_else(|| panic!("{what}: reference runtime unavailable"));
    assert_eq!(actual, "OK", "{what}: reference runtime");
}

fn method_body(dir: &std::path::Path, class: &str, method: &str) -> Vec<String> {
    let class_file = dir.join(format!("{class}.class"));
    let disassembly = common::javap(&["-c", "-p", &class_file.to_string_lossy()])
        .expect("javap unavailable for bottom-value parity");
    let mut body = Vec::new();
    let mut inside = false;
    for line in disassembly.lines() {
        if line.contains(method) && line.trim_end().ends_with(");") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.trim().is_empty() || line == "}" {
            break;
        }
        let trimmed = line.trim();
        if trimmed == "Code:" {
            continue;
        }
        let Some((_, instruction)) = trimmed.split_once(':') else {
            continue;
        };
        let mut normalized = String::new();
        let mut chars = instruction.trim().chars().peekable();
        while let Some(character) = chars.next() {
            normalized.push(character);
            if character == '#' {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
            }
        }
        let mut tokens = normalized.split_whitespace();
        let normalized = match tokens.next() {
            Some(mnemonic) if mnemonic.starts_with("if") || mnemonic == "goto" => {
                mnemonic.to_string()
            }
            Some(mnemonic) => std::iter::once(mnemonic)
                .chain(tokens)
                .collect::<Vec<_>>()
                .join(" "),
            None => String::new(),
        };
        body.push(normalized);
    }
    assert!(!body.is_empty(), "missing {class}.{method}:\n{disassembly}");
    body
}

fn compile_both_for_bytecode(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = common::scratch_dir().expect("allocate bottom-value bytecode fixture");
    let source = root.join("Bottom.kt");
    let krusty = root.join("krusty");
    let kotlinc = root.join("kotlinc");
    fs::create_dir_all(&krusty).expect("create krusty bytecode output");
    fs::create_dir_all(&kotlinc).expect("create kotlinc bytecode output");
    fs::write(&source, src).expect("write bottom-value bytecode fixture");

    let stdlib = common::stdlib_jar();
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().into_owned(),
        "-d".to_string(),
        kotlinc.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        stdlib.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler unavailable for bottom-value bytecode parity");
    assert_eq!(code, 0, "kotlinc rejected bottom-value fixture: {stderr}");

    for (internal, bytes) in common::expect_classes_with_stdlib(src, "Bottom") {
        let path = krusty.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create krusty class package");
        }
        fs::write(path, bytes).expect("write krusty class");
    }
    (krusty, kotlinc)
}

#[test]
fn nothing_call_in_else_branch() {
    const SRC: &str = "var flag = true\n\
fun exit(): Nothing = throw RuntimeException(\"boom\")\n\
fun box(): String {\n\
    var a: String\n\
    if (flag) { a = \"OK\" } else { exit() }\n\
    return a\n\
}\n";
    assert_eq!(run(SRC).expect("Nothing call in else branch"), "OK");
}

#[test]
fn nothing_call_in_if_expression_value() {
    const SRC: &str = "fun fail(): Nothing = throw RuntimeException(\"x\")\n\
fun pick(b: Boolean): String {\n\
    val s = if (b) \"yes\" else fail()\n\
    return s\n\
}\n\
fun box(): String = if (pick(true) == \"yes\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Nothing call in if-expression value"), "OK");
}

#[test]
fn not_null_assertion_of_null_terminates_a_boolean_branch() {
    const SRC: &str = "fun f(a: Int): Boolean = if (a > 0) true else null!!\n\
fun box(): String {\n\
    if (!f(1)) return \"fail: taken branch\"\n\
    val thrown = try { f(0); \"none\" } catch (e: NullPointerException) { \"npe\" }\n\
    return if (thrown == \"npe\") \"OK\" else \"fail: $thrown\"\n\
}\n";
    run_both(SRC, "null!! opposite a Boolean");
}

#[test]
fn not_null_assertion_of_null_terminates_every_primitive_width() {
    const SRC: &str = "fun i(a: Int) = if (a > 0) 7 else null!!\n\
fun l(a: Int) = if (a > 0) 7L else null!!\n\
fun d(a: Int) = if (a > 0) 7.5 else null!!\n\
fun c(a: Int) = if (a > 0) 'x' else null!!\n\
fun box(): String {\n\
    if (i(1) != 7) return \"fail: Int\"\n\
    if (l(1) != 7L) return \"fail: Long\"\n\
    if (d(1) != 7.5) return \"fail: Double\"\n\
    if (c(1) != 'x') return \"fail: Char\"\n\
    return \"OK\"\n\
}\n";
    run_both(SRC, "null!! opposite each primitive width");
}

#[test]
fn not_null_assertion_of_null_terminates_a_when_arm() {
    const SRC: &str = "fun f(a: Int): Int = when {\n\
    a > 0 -> 1\n\
    a < 0 -> -1\n\
    else -> null!!\n\
}\n\
fun box(): String {\n\
    if (f(3) != 1) return \"fail: positive\"\n\
    if (f(-3) != -1) return \"fail: negative\"\n\
    return \"OK\"\n\
}\n";
    run_both(SRC, "null!! in a when arm");
}

#[test]
fn a_reference_sibling_of_a_terminated_assertion_still_merges() {
    const SRC: &str = "fun f(a: Int): String = if (a > 0) \"y\" else null!!\n\
fun box(): String = if (f(1) == \"y\") \"OK\" else \"fail\"\n";
    run_both(SRC, "null!! opposite a reference");
}

#[test]
fn primitive_merge_bottom_completion_matches_kotlinc_bytecode_exactly() {
    const SRC: &str = "fun primitiveMerge(c: Boolean): Boolean = if (c) true else null!!\n\
fun ordinaryAssert(value: String?): String = value!!\n";
    let (krusty, kotlinc) = compile_both_for_bytecode(SRC);
    let ours = method_body(&krusty, "BottomKt", "primitiveMerge");
    let reference = method_body(&kotlinc, "BottomKt", "primitiveMerge");
    assert_eq!(ours, reference, "primitive bottom merge bytecode");
    assert_eq!(
        method_body(&krusty, "BottomKt", "ordinaryAssert"),
        method_body(&kotlinc, "BottomKt", "ordinaryAssert"),
        "ordinary non-null assertion must not gain bottom completion"
    );
}

#[test]
fn declared_and_inferred_external_nothing_calls_retain_bottom_completion() {
    common::Fixture::new()
        .reference_lib(
            "Lib.kt",
            "package lib\n\
             fun declared(): Nothing = throw IllegalStateException(\"declared\")\n\
             fun inferred() = throw IllegalArgumentException(\"inferred\")\n",
        )
        .assert_box_ok(
            "import lib.declared\n\
             import lib.inferred\n\
             fun box(): String {\n\
                 val declaredResult = try { declared(); \"fell through\" }\n\
                     catch (e: IllegalStateException) { e.message }\n\
                 val inferredResult = try { inferred(); \"fell through\" }\n\
                     catch (e: IllegalArgumentException) { e.message }\n\
                 return if (declaredResult == \"declared\" && inferredResult == \"inferred\")\n\
                     \"OK\" else \"fail: $declaredResult/$inferredResult\"\n\
             }\n",
        );
}
