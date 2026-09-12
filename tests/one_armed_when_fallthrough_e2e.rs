//! A one-armed `if` (a `when` with a single condition and no `else`) is a STATEMENT: when its body
//! finishes there is nothing left to skip, so kotlinc lets the body fall through to the instruction
//! the condition's false-branch already targets. krusty emitted a `goto` to that very instruction —
//! a dead jump in every such `if`, and one more `goto` than kotlinc in a great many methods.
//!
//! The shape also reaches generated code: the `@Serializable` deserialization constructor's
//! missing-element check is exactly this, and its bytes cannot match kotlinc's while the jump stands.
use super::common;

const SRC: &str = "fun report(x: Int): Int {\n\
                   \x20   var r = 0\n\
                   \x20   if (x > 0) r = 1\n\
                   \x20   return r\n\
                   }\n";

/// The instruction mnemonics of `report`, in order.
fn mnemonics(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .skip_while(|line| !line.contains("int report("))
        .skip(1)
        // javap's `//` comments carry descriptors full of `;`, so strip them before using a
        // trailing `;` to mean "the next member's declaration row".
        .map(|line| line.split("//").next().unwrap_or(line).trim().to_string())
        .take_while(|line| !line.ends_with(';'))
        .filter_map(|line| {
            let (pc, rest) = line.split_once(": ")?;
            pc.trim().parse::<u32>().ok()?;
            Some(rest.split_whitespace().next()?.to_string())
        })
        .collect()
}

#[test]
fn a_one_armed_if_falls_through_instead_of_jumping_to_its_own_end() {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: no scratch dir");
        return;
    };
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = dir.join("OneArmedIf.kt");
    std::fs::write(&source, SRC).expect("write fixture");

    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp_module_target(
        SRC,
        "OneArmedIf",
        &[],
        "main",
        Some(69),
    )
    .expect("krusty compiles the fixture");
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("krusty class directory");
        }
        std::fs::write(path, bytes).expect("write krusty class");
    }

    let Some(reference) = common::javap(&[
        "-p",
        "-c",
        "-cp",
        &reference_dir.to_string_lossy(),
        "OneArmedIfKt",
    ]) else {
        eprintln!("skipping: javap unavailable");
        return;
    };
    let krusty = common::javap(&[
        "-p",
        "-c",
        "-cp",
        &krusty_dir.to_string_lossy(),
        "OneArmedIfKt",
    ])
    .expect("javap reads krusty's output");
    let _ = std::fs::remove_dir_all(&dir);

    let want = mnemonics(&reference);
    assert!(
        want.iter().any(|m| m == "ifle"),
        "reference must branch over the body — that is the shape under test:\n{reference}"
    );
    assert_eq!(
        want.iter().filter(|m| *m == "goto").count(),
        0,
        "reference must not jump at the end of a one-armed if:\n{reference}"
    );
    let got = mnemonics(&krusty);
    assert_eq!(
        got.iter().filter(|m| *m == "goto").count(),
        0,
        "krusty jumps at the end of a one-armed if:\n{krusty}"
    );
}

/// An empty later BODY does not mean that falling into its branch emits nothing: its condition is
/// still observable. A selected earlier arm must jump over that condition.
#[test]
fn a_taken_branch_skips_the_condition_of_a_later_empty_branch() {
    const SOURCE: &str = "var calls = 0\n\
        fun sideCondition(): Boolean { calls += 1; return true }\n\
        fun choose(first: Boolean) {\n\
        \x20   when {\n\
        \x20       first -> Unit\n\
        \x20       sideCondition() -> {}\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   choose(true)\n\
        \x20   return if (calls == 0) \"OK\" else \"FAIL:condition called\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SOURCE, "TakenBranchSkipsLaterCondition");
}
