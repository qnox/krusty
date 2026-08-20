//! Cross-file top-level overload conflicts, including kotlinc's entry-point and file-private
//! exceptions. Every acceptance assertion is checked against the provisioned reference kotlinc so
//! a source-name shortcut cannot redefine what counts as an entry point.

use super::common;

fn diagnostics(sources: &[&str]) -> Vec<String> {
    let mut diags = common::front_end_diagnostics_files_with_stdlib(sources);
    diags.sort();
    diags
}

fn reference_accepts(tag: &str, sources: &[&str]) -> bool {
    let work = common::scratch_dir()
        .expect("allocate main-conflict reference fixture")
        .join(tag);
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create main-conflict reference output");
    let mut args = vec![
        "-nowarn".to_string(),
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
    ];
    for (index, source) in sources.iter().enumerate() {
        let path = work.join(format!("Source{index}.kt"));
        std::fs::write(&path, source).expect("write main-conflict reference source");
        args.push(path.to_string_lossy().into_owned());
    }
    let (code, _) = common::kotlinc_compile(&args).expect("reference kotlinc unavailable");
    let _ = std::fs::remove_dir_all(&work);
    code == 0
}

fn assert_diagnostics(actual: Vec<String>, expected: &[&str]) {
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|message| message.to_string())
            .collect::<Vec<_>>()
    );
}

fn assert_acceptance_matches_kotlinc(tag: &str, sources: &[&str], expected: bool) -> Vec<String> {
    assert_eq!(
        reference_accepts(tag, sources),
        expected,
        "{tag}: stale expected kotlinc result"
    );
    let diagnostics = diagnostics(sources);
    assert_eq!(
        diagnostics.is_empty(),
        expected,
        "{tag}: krusty diagnostics: {diagnostics:?}"
    );
    diagnostics
}

#[test]
fn cross_file_main_functions_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "array-main",
        &[
            "package p\nfun main(rawArgs: Array<String>) {}\n",
            "package p\nfun main(rawArgs: Array<String>) {}\n",
        ],
        true,
    );
}

#[test]
fn cross_file_parameterless_main_functions_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "parameterless-main",
        &["package p\nfun main() {}\n", "package p\nfun main() {}\n"],
        true,
    );
}

#[test]
fn cross_file_vararg_main_functions_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "vararg-main",
        &[
            "package p\nfun main(vararg args: String) {}\n",
            "package p\nfun main(vararg args: String) {}\n",
        ],
        true,
    );
}

#[test]
fn cross_file_defaulted_main_functions_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "defaulted-main",
        &[
            "package p\nfun main(args: Array<String> = emptyArray()) {}\n",
            "package p\nfun main(args: Array<String> = emptyArray()) {}\n",
        ],
        true,
    );
}

#[test]
fn cross_file_expression_body_unit_main_functions_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "expression-unit-main",
        &[
            "package p\nfun main() = println(\"a\")\n",
            "package p\nfun main() = println(\"b\")\n",
        ],
        true,
    );
}

#[test]
fn cross_file_suspend_main_functions_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "suspend-main",
        &[
            "package p\nsuspend fun main() {}\n",
            "package p\nsuspend fun main() {}\n",
        ],
        true,
    );
}

#[test]
fn cross_file_main_with_ordinary_parameter_conflicts() {
    let diags = assert_acceptance_matches_kotlinc(
        "int-main",
        &[
            "package p\nfun main(x: Int) {}\n",
            "package p\nfun main(x: Int) {}\n",
        ],
        false,
    );
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun main(x: Int)",
            "conflicting overloads:\nfun main(x: Int)",
        ],
    );
}

#[test]
fn cross_file_main_with_non_unit_return_conflicts() {
    let diags = assert_acceptance_matches_kotlinc(
        "returning-main",
        &[
            "package p\nfun main(): String = \"a\"\n",
            "package p\nfun main(): String = \"b\"\n",
        ],
        false,
    );
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun main(): String",
            "conflicting overloads:\nfun main(): String",
        ],
    );
}

#[test]
fn cross_file_generic_main_functions_conflict() {
    let diags = assert_acceptance_matches_kotlinc(
        "generic-main",
        &[
            "package p\nfun <T> main() {}\n",
            "package p\nfun <T> main() {}\n",
        ],
        false,
    );
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun <T> main()",
            "conflicting overloads:\nfun <T> main()",
        ],
    );
}

#[test]
fn entry_point_does_not_conflict_with_cross_file_ordinary_main() {
    assert_acceptance_matches_kotlinc(
        "entry-and-generic-main",
        &[
            "package p\nfun main() {}\n",
            "package p\nfun <T> main() {}\n",
        ],
        true,
    );
    assert_acceptance_matches_kotlinc(
        "generic-and-entry-main",
        &[
            "package p\nfun <T> main() {}\n",
            "package p\nfun main() {}\n",
        ],
        true,
    );
}

#[test]
fn same_file_main_duplicates_still_conflict() {
    let diags = diagnostics(&[
        "package p\nfun main(rawArgs: Array<String>) {}\nfun main(rawArgs: Array<String>) {}\n",
    ]);
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun main(rawArgs: Array<String>)",
            "conflicting overloads:\nfun main(rawArgs: Array<String>)",
        ],
    );
}

#[test]
fn same_file_entry_point_and_ordinary_main_conflict() {
    let diags = assert_acceptance_matches_kotlinc(
        "same-file-entry-and-generic-main",
        &["package p\nfun main() {}\nfun <T> main() {}\n"],
        false,
    );
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun <T> main()",
            "conflicting overloads:\nfun main()",
        ],
    );
}

#[test]
fn nested_and_local_main_functions_do_not_join_the_top_level_scope() {
    assert_acceptance_matches_kotlinc(
        "nested-and-local-main",
        &[
            "package p\nfun main() {}\nfun holder() { fun main() {} }\n",
            "package p\nclass Runner { fun main() {} }\n",
        ],
        true,
    );
}

#[test]
fn cross_file_non_main_duplicates_still_conflict() {
    let diags = assert_acceptance_matches_kotlinc(
        "public-helper",
        &[
            "package p\nfun helper(rawArgs: Array<String>) {}\n",
            "package p\nfun helper(rawArgs: Array<String>) {}\n",
        ],
        false,
    );
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun helper(rawArgs: Array<String>)",
            "conflicting overloads:\nfun helper(rawArgs: Array<String>)",
        ],
    );
}

#[test]
fn cross_file_private_functions_with_any_name_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "private-helper",
        &[
            "package p\nprivate fun helper(x: Int) {}\n",
            "package p\nprivate fun helper(x: Int) {}\n",
        ],
        true,
    );
}

#[test]
fn cross_file_public_and_private_functions_conflict_in_the_private_file() {
    let diags = assert_acceptance_matches_kotlinc(
        "mixed-helper-visibility",
        &[
            "package p\nfun helper(x: Int) {}\n",
            "package p\nprivate fun helper(x: Int) {}\n",
        ],
        false,
    );
    assert_diagnostics(diags, &["conflicting overloads:\nfun helper(x: Int)"]);
    let reversed = assert_acceptance_matches_kotlinc(
        "mixed-helper-visibility-reversed",
        &[
            "package p\nprivate fun helper(x: Int) {}\n",
            "package p\nfun helper(x: Int) {}\n",
        ],
        false,
    );
    assert_diagnostics(reversed, &["conflicting overloads:\nfun helper(x: Int)"]);
}

#[test]
fn cross_file_private_extension_functions_do_not_conflict() {
    assert_acceptance_matches_kotlinc(
        "private-extension",
        &[
            "package p\nprivate fun String.helper() {}\n",
            "package p\nprivate fun String.helper() {}\n",
        ],
        true,
    );
}

#[test]
fn same_file_private_extension_functions_conflict() {
    let diags = assert_acceptance_matches_kotlinc(
        "same-file-private-extension",
        &["package p\nprivate fun String.helper() {}\nprivate fun String.helper() {}\n"],
        false,
    );
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun String.helper()",
            "conflicting overloads:\nfun String.helper()",
        ],
    );
}

#[test]
fn cross_file_public_extension_functions_conflict() {
    let diags = assert_acceptance_matches_kotlinc(
        "public-extension",
        &[
            "package p\nfun String.helper() {}\n",
            "package p\nfun String.helper() {}\n",
        ],
        false,
    );
    assert_diagnostics(
        diags,
        &[
            "conflicting overloads:\nfun String.helper()",
            "conflicting overloads:\nfun String.helper()",
        ],
    );
}

#[test]
fn cross_file_public_and_private_extension_functions_conflict() {
    let diags = assert_acceptance_matches_kotlinc(
        "mixed-extension-visibility",
        &[
            "package p\nfun String.helper() {}\n",
            "package p\nprivate fun String.helper() {}\n",
        ],
        false,
    );
    assert_diagnostics(diags, &["conflicting overloads:\nfun String.helper()"]);
    let reversed = assert_acceptance_matches_kotlinc(
        "mixed-extension-visibility-reversed",
        &[
            "package p\nprivate fun String.helper() {}\n",
            "package p\nfun String.helper() {}\n",
        ],
        false,
    );
    assert_diagnostics(reversed, &["conflicting overloads:\nfun String.helper()"]);
}
