//! Declares an integration-test target's modules from the files in its package's `tests/` directory.
//!
//! A hand-kept `mod` list makes every pull request that adds a test edit the same file, so parallel
//! pull requests conflict on it, and a file whose declaration is lost compiles nowhere and never runs.
//! `declare_test_modules!("_e2e.rs")` instead declares one module per `tests/*_e2e.rs` file, named after
//! the file stem, in file-name order.
//!
//! Cargo records the files rustc read, not the directory listing, so adding or removing a file does not
//! by itself rebuild the target. The macro therefore also emits a test that lists the directory again at
//! run time and fails when the compiled module set is out of date.

use proc_macro::TokenStream;
use std::path::{Path, PathBuf};

/// Declares `#[path = "<tests>/<stem><suffix>"] mod <stem><suffix-stem>;` for every file in the calling
/// package's `tests/` directory whose name ends with the given suffix, plus a currency test.
#[proc_macro]
pub fn declare_test_modules(input: TokenStream) -> TokenStream {
    let suffix = match string_literal(&input) {
        Some(suffix) if suffix.ends_with(".rs") && suffix.len() > ".rs".len() => suffix,
        _ => return compile_error("expected a file-name suffix such as \"_e2e.rs\""),
    };
    let tests_dir = match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(manifest_dir) => PathBuf::from(manifest_dir).join("tests"),
        None => return compile_error("CARGO_MANIFEST_DIR is not set; run through cargo"),
    };
    let files = match suffixed_files(&tests_dir, &suffix) {
        Ok(files) => files,
        Err(error) => {
            return compile_error(&format!("cannot list {}: {error}", tests_dir.display()));
        }
    };

    let mut out = String::new();
    for file in &files {
        let path = tests_dir.join(file);
        out.push_str(&format!(
            "#[path = {path:?}] mod {};\n",
            &file[..file.len() - ".rs".len()]
        ));
    }
    out.push_str(&currency_test(&tests_dir, &suffix, &files));
    out.parse()
        .expect("generated module declarations are valid Rust")
}

/// The sorted names of the files in `dir` that end with `suffix`.
fn suffixed_files(dir: &Path, suffix: &str) -> std::io::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.ends_with(suffix) {
            files.push(name);
        }
    }
    files.sort();
    Ok(files)
}

/// A test that lists `dir` again when it runs and compares it with the modules compiled in.
fn currency_test(dir: &Path, suffix: &str, files: &[String]) -> String {
    format!(
        r#"
#[test]
fn declared_test_modules_match_the_tests_directory() {{
    const COMPILED: &[&str] = &{files:?};
    let dir = ::std::path::Path::new({dir:?});
    let mut present: ::std::vec::Vec<::std::string::String> = ::std::fs::read_dir(dir)
        .expect("list the tests directory")
        .map(|entry| entry.expect("read a tests directory entry"))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with({suffix:?}))
        .collect();
    present.sort();
    assert_eq!(
        present, COMPILED,
        "{{}} gained or lost a `*{{}}` file since this target was compiled; Cargo does not track the \
         directory listing, so touch the target's root file and rebuild",
        dir.display(),
        {suffix:?},
    );
}}
"#,
        dir = dir.to_string_lossy(),
    )
}

fn string_literal(input: &TokenStream) -> Option<String> {
    let mut tokens = input.clone().into_iter();
    let literal = tokens.next()?.to_string();
    if tokens.next().is_some() {
        return None;
    }
    literal
        .strip_prefix('"')?
        .strip_suffix('"')
        .filter(|body| !body.contains(['\\', '"']))
        .map(str::to_owned)
}

fn compile_error(message: &str) -> TokenStream {
    format!("::core::compile_error!({message:?});")
        .parse()
        .expect("compile_error! invocation is valid Rust")
}
