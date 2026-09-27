//! kotlinc maps a `Unit` lambda's implicit return to the literal's closing `}`, and the implicit
//! return of a lambda's value to nothing of its own: it stays on the line of the value.

use super::common;

/// `javap -c -l` of `class` from both compilers, pool indices normalized, must be equal.
fn expect_code_and_lines_match(name: &str, src: &str, class: &str) {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference = dir.join("ref");
    let actual = dir.join("krusty");
    std::fs::create_dir_all(&reference).unwrap();
    std::fs::create_dir_all(&actual).unwrap();
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).unwrap();
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp(src, name, &[common::stdlib_jar()])
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));
    std::fs::write(actual.join(format!("{class}.class")), bytes).unwrap();
    let disassemble = |dir: &std::path::Path| {
        let path = dir.join(format!("{class}.class"));
        common::javap(&["-c", "-l", "-p", &path.to_string_lossy()])
            .expect("pooled javap")
            .lines()
            .filter(|line| !line.starts_with("Compiled from"))
            .map(|line| {
                let code = line.split("//").next().unwrap_or(line);
                code.split_whitespace()
                    .map(|token| if token.starts_with('#') { "#" } else { token })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (want, got) = (disassemble(&reference), disassemble(&actual));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        got, want,
        "{name}: instructions and line tables must be kotlinc's"
    );
}

#[test]
fn a_unit_lambdas_implicit_return_is_on_its_closing_brace() {
    let src = "var res = \"\"\n\
fun run(f: () -> Unit) = f()\n\
fun box(): String {\n\
    run {\n\
        res = \"OK\"\n\
    }\n\
    return res\n\
}\n";
    expect_code_and_lines_match("UnitLambdaClose", src, "UnitLambdaCloseKt");
}

#[test]
fn a_lambdas_value_returns_on_the_values_own_line() {
    let src = "var x = 1\n\
fun run(f: () -> String) = f()\n\
fun box(): String {\n\
    return run {\n\
        \"OK\" + x\n\
    }\n\
}\n";
    expect_code_and_lines_match("ValueLambdaReturn", src, "ValueLambdaReturnKt");
}

#[test]
fn a_lambdas_branch_value_returns_without_a_closing_brace_line() {
    let src = "var x = 1\n\
fun run(f: () -> String) = f()\n\
fun box(): String {\n\
    return run {\n\
        if (x > 0) \"OK\" else \"no\"\n\
    }\n\
}\n";
    expect_code_and_lines_match("BranchLambdaReturn", src, "BranchLambdaReturnKt");
}
