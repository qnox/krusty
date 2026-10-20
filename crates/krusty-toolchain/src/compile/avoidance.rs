//! Whether a module's compilation can be skipped because its inputs and its output are exactly as
//! its last successful compilation left them.
//!
//! The inputs are fingerprinted by krusty itself (`krusty compilation-fingerprint` before the
//! compile arguments): krusty reads the arguments as the compilation does and digests what that
//! compilation selects. The output directory is compared by its files' paths and contents. Nothing
//! is identified by a size or a modification time, and when the fingerprint cannot be taken the
//! module compiles. This is the check krusty's Build Tools API implementation makes for the
//! JetBrains toolchain (`CompileAvoidance`), with the same state: the fingerprint, then the
//! output's digest.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

/// The fingerprint of the inputs krusty's compilation with `arguments` reads, if krusty takes one.
pub fn inputs(krusty: &Path, arguments: &[OsString]) -> Option<String> {
    let ran = Command::new(krusty)
        .arg("compilation-fingerprint")
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let fingerprint = String::from_utf8(ran.stdout).ok()?;
    let fingerprint = fingerprint.trim();
    let is_digest = fingerprint.len() == 64
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    (ran.status.success() && is_digest).then(|| fingerprint.to_string())
}

/// Whether the last successful compilation, recorded in `state`, had `inputs` and left `output`
/// as it is now.
pub fn up_to_date(state: &Path, output: &Path, inputs: &str) -> bool {
    let Ok(recorded) = std::fs::read_to_string(state) else {
        return false;
    };
    output.is_dir()
        && output_contents(output).is_ok_and(|contents| recorded == record_text(inputs, &contents))
}

/// Forgets the last compilation, before one that may fail or be interrupted midway.
pub fn forget(state: &Path) -> io::Result<()> {
    match std::fs::remove_file(state) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Records a successful compilation with `inputs` and the `output` it left.
pub fn record(state: &Path, output: &Path, inputs: &str) -> io::Result<()> {
    let contents = output_contents(output)?;
    if let Some(parent) = state.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(state, record_text(inputs, &contents))
}

fn record_text(inputs: &str, contents: &str) -> String {
    format!("{inputs}\n{contents}\n")
}

/// Every file below `output`, by `/`-separated relative path and contents, as one digest.
fn output_contents(output: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    walk(output, "", &mut files)?;
    files.sort();
    let mut tree = Sha256::new();
    for relative in &files {
        field(&mut tree, relative.as_bytes());
        let contents = Sha256::digest(std::fs::read(output.join(relative))?);
        field(&mut tree, &contents);
    }
    Ok(hex(&tree.finalize()))
}

fn walk(directory: &Path, prefix: &str, files: &mut Vec<String>) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), &format!("{name}/"), files)?;
        } else {
            files.push(name);
        }
    }
    Ok(())
}

/// Length-delimited, so no value can be mistaken for a delimiter.
fn field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u32).to_be_bytes());
    digest.update(value);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "krusty-toolchain-avoidance-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("output/p")).unwrap();
        std::fs::write(directory.join("output/p/A.class"), b"a").unwrap();
        directory
    }

    #[test]
    fn a_recorded_compilation_is_up_to_date_until_its_inputs_or_output_change() {
        let directory = scratch("record");
        let (state, output) = (
            directory.join("state/compilation.state"),
            directory.join("output"),
        );
        let inputs = "a".repeat(64);
        assert!(!up_to_date(&state, &output, &inputs), "nothing recorded");
        record(&state, &output, &inputs).unwrap();
        assert!(up_to_date(&state, &output, &inputs));
        assert!(
            !up_to_date(&state, &output, &"b".repeat(64)),
            "other inputs"
        );
        std::fs::write(output.join("p/A.class"), b"changed").unwrap();
        assert!(!up_to_date(&state, &output, &inputs), "a changed class");
        std::fs::write(output.join("p/A.class"), b"a").unwrap();
        assert!(up_to_date(&state, &output, &inputs), "the class restored");
        std::fs::write(output.join("B.class"), b"b").unwrap();
        assert!(!up_to_date(&state, &output, &inputs), "an added class");
        std::fs::remove_file(output.join("B.class")).unwrap();
        forget(&state).unwrap();
        assert!(!up_to_date(&state, &output, &inputs), "forgotten");
        forget(&state).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_renamed_class_changes_the_output_digest() {
        let directory = scratch("rename");
        let output = directory.join("output");
        let before = output_contents(&output).unwrap();
        std::fs::rename(output.join("p/A.class"), output.join("p/B.class")).unwrap();
        assert_ne!(output_contents(&output).unwrap(), before);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
