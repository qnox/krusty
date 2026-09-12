//! A modifier SOFT KEYWORD used as a NAME at the end of a bare `if`/`when` branch must close that
//! branch — the declaration on the next line belongs to the enclosing scope.
//!
//! `fun f(value: String) = if (c) "lit" else value` followed by another declaration: the statement
//! parser's modifier-prefix scan looks ACROSS newlines for a declaration keyword — it has to, since
//! `suspend`⏎`fun local()` inside a block is one declaration — and so it read `value`⏎`fun g(…)` as
//! a modifier-prefixed local function. The branch then carried a declaration's `Unit` value, the
//! `if` joined `String` with `Unit` to `Any`, and the enclosing function reported a return-type
//! mismatch whose span covered the swallowed declaration.
//!
//! `value` is the soft keyword that matters in practice (`value class`), and it is an ordinary
//! parameter name — rename it and the same code compiles, which is what makes the diagnostic so
//! misleading. Everything the swallowed declaration provided disappears too, so the real errors are
//! the unresolved references reported against its CALLERS elsewhere in the file.
use super::common;

fn assert_compiles_like_kotlinc(src: &str, stem: &str) {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let reference = common::scratch_dir().expect("scratch directory");
    let source = reference.join(format!("{stem}.kt"));
    let output = reference.join("reference");
    std::fs::write(&source, src).expect("write reference source");
    std::fs::create_dir_all(&output).expect("create reference output directory");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-cp".to_string(),
        stdlib.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    assert_eq!(code, 0, "kotlinc must accept the fixture: {stderr}");
    assert!(
        common::compile_in_process(src, stem, &[stdlib], Some(jdk.as_path())).is_some(),
        "krusty must accept the same fixture as kotlinc"
    );
    let _ = std::fs::remove_dir_all(reference);
}

#[test]
fn a_soft_keyword_name_ends_a_bare_branch() {
    const SRC: &str = "class Holder {\n\
        \x20   private fun pad(value: String): String =\n\
        \x20       if (value.length > 2) \"lit\" else value\n\
        \x20\n\
        \x20   fun use(v: String): String = pad(v)\n\
        }\n";
    assert_compiles_like_kotlinc(SRC, "SoftKeywordBranch");
}

/// The swallowed declaration is the real casualty: its callers lose it. This is the corpus shape —
/// the reported errors named a helper and a property that the file plainly declares.
#[test]
fn the_declaration_after_the_branch_stays_visible() {
    const SRC: &str = "class Holder {\n\
        \x20   private fun pad(value: String): String =\n\
        \x20       if (value.length > 2) \"lit\" else value\n\
        \x20\n\
        \x20   private fun helper(text: String): String = text.trim()\n\
        \x20\n\
        \x20   fun use(v: String): String = helper(pad(v))\n\
        }\n";
    assert_compiles_like_kotlinc(SRC, "SoftKeywordBranchNeighbour");
}

/// `parse_branch` is shared by `if` and `when`; keep the latter covered with a second modifier
/// spelling so the fix cannot accidentally become a special case for `value` or `else`.
#[test]
fn a_second_soft_keyword_name_ends_a_when_branch() {
    const SRC: &str = "class Holder {\n\
        \x20   private fun pick(actual: String): String = when (actual.length) {\n\
        \x20       0 -> \"empty\"\n\
        \x20       else -> actual\n\
        \x20   }\n\
        \x20\n\
        \x20   fun use(v: String): String = pick(v)\n\
        }\n";
    assert_compiles_like_kotlinc(SRC, "SoftKeywordWhenBranch");
}

/// The scan must still cross a newline where Kotlin does: a modifier on its own line inside a BLOCK
/// prefixes the declaration that follows (kotlinc compiles this as a suspend local function).
#[test]
fn a_modifier_on_its_own_line_still_prefixes_a_local_declaration() {
    const SRC: &str = "fun holder(): Int {\n\
        \x20   suspend\n\
        \x20   fun local(): Int = 1\n\
        \x20   return 2\n\
        }\n";
    assert_compiles_like_kotlinc(SRC, "ModifierOwnLine");
}
