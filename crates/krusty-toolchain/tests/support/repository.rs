//! The local Maven repository every resolving case shares, `tests/cases/dependencies/repository`.

/// The files of the local Maven repository every resolving case shares
/// (`tests/cases/dependencies/repository`), by `/`-separated path below it, with `{kotlin}` spelled
/// in their paths and contents.
pub fn shared() -> Vec<(String, String)> {
    fn below(directory: &std::path::Path, prefix: &str, files: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = format!(
                "{prefix}{}",
                crate::support::with_kotlin_version(&entry.file_name().to_string_lossy())
            );
            if entry.file_type().unwrap().is_dir() {
                below(&entry.path(), &format!("{name}/"), files);
            } else {
                let text = std::fs::read_to_string(entry.path()).unwrap();
                files.push((name, crate::support::with_kotlin_version(&text)));
            }
        }
    }
    let mut files = Vec::new();
    below(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/cases/dependencies/repository"),
        "",
        &mut files,
    );
    files
}
