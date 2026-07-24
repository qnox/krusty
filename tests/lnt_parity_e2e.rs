//! LineNumberTable byte-parity: krusty's per-statement line mapping must match kotlinc's exactly.
//! Every case here is PARAMLESS with no locals or top-level properties, so kotlinc emits no
//! LocalVariableTable — the class files must therefore be BYTE-IDENTICAL once the
//! LineNumberTable matches (code shapes for these forms already are). LVT parity is a separate,
//! follow-up slice.
//!
//! kotlinc's mapping (probed on 2.4.0): one entry per statement at its first pc; an expression
//! body maps to the expression's line; a `Unit` fn's implicit `return` maps to the CLOSING-BRACE
//! line; a loop back-edge/increment re-marks the loop-header line; branch statements map to their
//! own lines. (Inline-function SMAP lines are out of scope here.)

use std::fs;

use super::common;

/// Compile `src` with both compilers and byte-compare the named facade class. Returns
/// `None` (skip) when the reference toolchain is unavailable.
fn byte_diff(name: &str, src: &str, class: &str) -> Option<Result<(), String>> {
    let dir = std::env::temp_dir().join(format!("krusty_lntp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let kref = dir.join("ref");
    fs::create_dir_all(&kref).ok()?;
    let src_path = dir.join(format!("{name}.kt"));
    fs::write(&src_path, src).ok()?;
    let args = vec![
        "-d".to_string(),
        kref.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let ref_bytes = fs::read(kref.join(format!("{class}.class"))).ok()?;

    // The metadata_cp variant stamps `SourceFile` (`<stem>.kt`) exactly as the CLI backend does, so
    // the emitted bytes equal a `krusty -d …` run — the byte-identity codepath.
    let classes = common::compile_in_process_metadata_cp(src, name, &[])
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, krusty_bytes) = classes
        .iter()
        .find(|(n, _)| n == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));

    let _ = fs::remove_dir_all(&dir);
    if krusty_bytes == &ref_bytes {
        return Some(Ok(()));
    }
    let off = krusty_bytes
        .iter()
        .zip(ref_bytes.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| krusty_bytes.len().min(ref_bytes.len()));
    Some(Err(format!(
        "{name}/{class}: bytes differ at offset {off} (krusty {} B, kotlinc {} B)",
        krusty_bytes.len(),
        ref_bytes.len()
    )))
}

fn assert_byte_identical(name: &str, src: &str, class: &str) {
    match byte_diff(name, src, class) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// Weaker check for shapes with a KNOWN residual byte divergence outside the code/line tables:
/// `javap -c -l` (instructions + LineNumberTable + LocalVariableTable) must compare equal.
fn assert_code_and_lnt_identical(name: &str, src: &str, class: &str) {
    assert_code_and_lnt_impl(name, src, class, false);
}

/// Like [`assert_code_and_lnt_identical`] but additionally strips `LocalVariableTable` blocks —
/// for shapes where kotlinc emits an LVT krusty doesn't yet (params/locals).
fn assert_lnt_identical_sans_lvt(name: &str, src: &str, class: &str) {
    assert_code_and_lnt_impl(name, src, class, true);
}

fn assert_code_and_lnt_impl(name: &str, src: &str, class: &str, strip_lvt: bool) {
    let Some(jh) = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())
    else {
        eprintln!("skip ({name}: JAVA_HOME unavailable)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("krusty_lntp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let kref = dir.join("ref");
    let krout = dir.join("out");
    fs::create_dir_all(&kref).unwrap();
    fs::create_dir_all(&krout).unwrap();
    let src_path = dir.join(format!("{name}.kt"));
    fs::write(&src_path, src).unwrap();
    let args = vec![
        "-d".to_string(),
        kref.to_string_lossy().into_owned(),
        src_path.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skip ({name}: kotlinc unavailable)");
        return;
    };
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp(src, name, &[])
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));
    fs::write(krout.join(format!("{class}.class")), bytes).unwrap();
    let javap = |p: &std::path::Path| -> String {
        let out = std::process::Command::new(format!("{jh}/bin/javap"))
            .args(["-c", "-l", "-p"])
            .arg(p)
            .output()
            .expect("javap runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with("Compiled from"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let kr = javap(&krout.join(format!("{class}.class")));
    let ko = javap(&kref.join(format!("{class}.class")));
    let _ = fs::remove_dir_all(&dir);
    if strip_lvt {
        // LocalVariableTable emission is a separate, not-yet-landed slice — drop those blocks so
        // the comparison pins instructions + LineNumberTable only.
        let strip = |s: &str| -> String {
            let mut out = Vec::new();
            let mut in_lvt = false;
            for l in s.lines() {
                let trimmed = l.trim_start();
                if trimmed.starts_with("LocalVariableTable:") {
                    in_lvt = true;
                    continue;
                }
                if in_lvt {
                    // The block is the header + column line + indented rows; any less-indented
                    // attribute/method line ends it.
                    if trimmed.starts_with("Start ")
                        || trimmed
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_digit() || c == '-')
                    {
                        continue;
                    }
                    in_lvt = false;
                }
                out.push(l);
            }
            out.join("\n")
        };
        assert_eq!(
            strip(&kr),
            strip(&ko),
            "{name}: instructions + LineNumberTable must match kotlinc"
        );
    } else {
        assert_eq!(
            kr, ko,
            "{name}: instructions + line tables must match kotlinc"
        );
    }
}

#[test]
fn single_return_fn() {
    assert_byte_identical(
        "lntSingle",
        "fun box(): String {\n    return \"OK\"\n}\n",
        "LntSingleKt",
    );
}

#[test]
fn multi_statement_fn() {
    assert_byte_identical(
        "lntMulti",
        "fun act() {\n}\n\
fun one(): Int {\n    return 1\n}\n\
fun multiStmt(): Int {\n    act()\n    act()\n    return one()\n}\n",
        "LntMultiKt",
    );
}

#[test]
fn expression_body_fn() {
    assert_byte_identical(
        "lntExpr",
        "fun one(): Int {\n    return 1\n}\nfun exprBody(): Int = one() + 1\n",
        "LntExprKt",
    );
}

#[test]
fn unit_fn_implicit_return_maps_to_closing_brace() {
    assert_byte_identical(
        "lntUnit",
        "fun act() {\n}\n\
fun unitTail() {\n    act()\n    act()\n}\n",
        "LntUnitKt",
    );
}

#[test]
fn branchy_fn() {
    assert_byte_identical(
        "lntBranch",
        "fun cond(): Boolean {\n    return false\n}\n\
fun branchy(): Int {\n    if (cond()) {\n        return 1\n    } else {\n        return 2\n    }\n}\n",
        "LntBranchKt",
    );
}

#[test]
fn while_loop_fn() {
    // NOT byte-identical yet: krusty emits one extra `same` StackMapTable frame at the loop head
    // (a pre-existing loop-shape divergence, tracked separately). The instructions AND the
    // LineNumberTable must already match, so compare the `javap -c -l` text instead of the bytes.
    assert_code_and_lnt_identical(
        "lntLoop",
        "fun cond(): Boolean {\n    return false\n}\n\
fun act() {\n}\n\
fun loopy(): Int {\n    while (cond()) {\n        act()\n    }\n    return 3\n}\n",
        "LntLoopKt",
    );
}

#[test]
fn guarded_param_fn_lnt() {
    // A non-null reference param gets a `checkNotNullParameter` prologue; kotlinc's LVT covers the
    // param (not yet emitted by krusty for top-level fns), so only instructions + LNT are pinned —
    // including WHERE the first line entry starts relative to the prologue.
    assert_lnt_identical_sans_lvt(
        "lntParam",
        "fun act() {\n}\n\
fun len2(s: String): Int {\n    act()\n    return 5\n}\n",
        "LntParamKt",
    );
}

#[test]
fn member_method_lnt() {
    // A class member method: marks must not fight the declared-method debug fallback.
    assert_lnt_identical_sans_lvt(
        "lntMember",
        "fun act() {\n}\n\
class C {\n    fun m(): Int {\n        act()\n        return 1\n    }\n}\n",
        "C",
    );
}

#[test]
fn mid_return_fn() {
    assert_byte_identical(
        "lntMidRet",
        "fun cond(): Boolean {\n    return false\n}\n\
fun act() {\n}\n\
fun midRet(): Int {\n    if (cond()) {\n        return 7\n    }\n    act()\n    return 8\n}\n",
        "LntMidRetKt",
    );
}
