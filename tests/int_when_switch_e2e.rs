//! A `when` over `Int` constants compiles to a JVM switch. kotlinc picks `tableswitch` when the keys
//! are dense — the table spans at most twice as many slots as there are keys — and `lookupswitch`
//! otherwise; a single key is a plain comparison, no switch at all.
//!
//! krusty emitted a chain of `if_icmpne` comparisons for every shape, so no class containing a `when`
//! over constants could match kotlinc's code array.
use super::common;

const SRC: &str = "fun one(x: Int): String = when (x) { 0 -> \"a\"; else -> \"z\" }\n\
                   fun dense(x: Int): String = when (x) { 0 -> \"a\"; 1 -> \"b\"; 2 -> \"c\"; else -> \"z\" }\n\
                   fun edge(x: Int): String = when (x) { 0 -> \"a\"; 1 -> \"b\"; 2 -> \"c\"; 7 -> \"d\"; else -> \"z\" }\n\
                   fun sparse(x: Int): String = when (x) { 0 -> \"a\"; 100 -> \"b\"; 5000 -> \"c\"; else -> \"z\" }\n\
                   fun negative(x: Int): String = when (x) { -2 -> \"a\"; -1 -> \"b\"; 0 -> \"c\"; else -> \"z\" }\n\
                   fun box(): String = if (one(0) == \"a\" && one(9) == \"z\" &&\n\
                       dense(1) == \"b\" && dense(9) == \"z\" && edge(7) == \"d\" &&\n\
                       sparse(100) == \"b\" && sparse(9) == \"z\" && negative(-1) == \"b\" &&\n\
                       negative(9) == \"z\") \"OK\" else \"FAIL\"\n";

/// The switch header of `method`, as javap prints it: `tableswitch { // 0 to 2` or
/// `lookupswitch { // 3`, plus the key column of the table that follows. Offsets are dropped — they
/// depend on the bodies, which this test does not compare.
fn switch_shape(disassembly: &str, method: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut in_table = false;
    for raw in disassembly.lines() {
        let line = raw.trim();
        if !inside {
            inside = line.contains(&format!(" {method}(int);"));
            continue;
        }
        if line.ends_with(';') && line.contains('(') && !line.starts_with("descriptor:") {
            break;
        }
        if let Some(header) = line.split(": ").nth(1) {
            if header.starts_with("tableswitch") || header.starts_with("lookupswitch") {
                out.push(header.split_whitespace().collect::<Vec<_>>().join(" "));
                in_table = true;
                continue;
            }
        }
        if in_table {
            if line == "}" {
                in_table = false;
                continue;
            }
            // `      -1: 120` / `default: 165` — keep the key, drop the offset.
            if let Some((key, _)) = line.split_once(": ") {
                out.push(key.trim().to_string());
            }
        }
    }
    out
}

fn build_both() -> Option<(String, String)> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = dir.join("IntWhenSwitch.kt");
    std::fs::write(&source, SRC).expect("write fixture");

    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp_module_target(
        SRC,
        "IntWhenSwitch",
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

    let reference = common::javap(&[
        "-p",
        "-c",
        "-cp",
        &reference_dir.to_string_lossy(),
        "IntWhenSwitchKt",
    ])?;
    let krusty = common::javap(&[
        "-p",
        "-c",
        "-cp",
        &krusty_dir.to_string_lossy(),
        "IntWhenSwitchKt",
    ])
    .expect("javap reads krusty's output");
    let _ = std::fs::remove_dir_all(&dir);
    Some((reference, krusty))
}

#[test]
fn a_when_over_int_constants_switches_the_way_kotlinc_does() {
    let Some((reference, krusty)) = build_both() else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let expected = [
        ("one", Vec::<String>::new()),
        (
            "dense",
            ["tableswitch { // 0 to 2", "0", "1", "2", "default"]
                .map(String::from)
                .to_vec(),
        ),
        (
            "edge",
            [
                "tableswitch { // 0 to 7",
                "0",
                "1",
                "2",
                "3",
                "4",
                "5",
                "6",
                "7",
                "default",
            ]
            .map(String::from)
            .to_vec(),
        ),
        (
            "sparse",
            ["lookupswitch { // 3", "0", "100", "5000", "default"]
                .map(String::from)
                .to_vec(),
        ),
        (
            "negative",
            ["tableswitch { // -2 to 0", "-2", "-1", "0", "default"]
                .map(String::from)
                .to_vec(),
        ),
    ];
    for (method, want) in expected {
        assert_eq!(
            switch_shape(&reference, method),
            want,
            "{method}: exact kotlinc switch shape\n{reference}"
        );
        assert_eq!(
            switch_shape(&krusty, method),
            want,
            "{method}: exact krusty switch shape\n{krusty}"
        );
    }
}

#[test]
fn int_switch_cases_and_defaults_execute() {
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "IntWhenSwitch")
            .expect("switch fixture compiles and runs"),
        "OK"
    );
}

/// kotlinc reads a `when` subject that is already a local — a parameter here — straight into the
/// dispatch (`iload_0; tableswitch`). krusty copied every subject into a temporary first, so even a
/// `when` that now switches correctly carried two extra instructions and an extra local ahead of it.
///
/// With the temporary gone the whole method matches kotlinc instruction for instruction, which is the
/// point: a switch with the right keys is still not the same code array.
#[test]
fn a_when_over_a_local_subject_matches_kotlinc_instruction_for_instruction() {
    let Some((reference, krusty)) = build_both() else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // Instruction rows of `method`, pool indices erased (their numbering is an emission-order
    // artifact) and javap's trailing comments dropped. Branch targets are kept.
    let instructions = |text: &str, method: &str| {
        let mut out = Vec::new();
        let mut inside = false;
        for raw in text.lines() {
            let line = raw.trim();
            if !inside {
                inside = line.contains(&format!(" {method}(int);"));
                continue;
            }
            if line.ends_with(';') && line.contains('(') {
                break;
            }
            let code = line.split("//").next().unwrap_or(line).trim();
            let normalized = code
                .split_whitespace()
                .map(|token| if token.starts_with('#') { "#" } else { token })
                .collect::<Vec<_>>()
                .join(" ");
            if !normalized.is_empty() {
                out.push(normalized);
            }
        }
        out
    };
    for method in ["dense", "sparse", "negative"] {
        let want = instructions(&reference, method);
        assert_eq!(
            instructions(&krusty, method),
            want,
            "{method}: instructions\n{krusty}"
        );
    }
}

#[test]
fn a_mutable_subject_keeps_its_original_snapshot() {
    const SOURCE: &str = "fun box(): String {\n\
        var x = 0\n\
        return when (x) {\n\
            ++x -> \"wrong-1\"\n\
            1 -> \"wrong-2\"\n\
            else -> \"OK\"\n\
        }\n\
    }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "MutableWhenSubject")
            .expect("mutable when subject compiles and runs"),
        "OK"
    );
}
