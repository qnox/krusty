//! Dependency fixtures built by the reference kotlinc.

use std::path::PathBuf;

use super::{kotlinc_compile, language_directives, scratch_dir, stdlib_jar};

/// Compile a dependency source set with the REFERENCE kotlinc (pooled server) into a scratch
/// classpath dir. `None` = toolchain unavailable; kotlinc REJECTING the sources panics — the
/// fixture is invalid Kotlin, which must never read as a skip. Each source's `// LANGUAGE:`
/// directives reach kotlinc as the `-XXLanguage:` flags krusty reads from the same directives.
pub(super) fn kotlinc_lib_out(sources: &[(&str, &str)]) -> Option<PathBuf> {
    let stdlib = stdlib_jar();
    let work = scratch_dir()?;
    let out = work.join("libout");
    std::fs::create_dir_all(&out).ok()?;
    let mut args = vec![
        "-d".into(),
        out.to_string_lossy().into_owned(),
        "-cp".into(),
        stdlib.to_string_lossy().into_owned(),
    ];
    let mut language = Vec::new();
    for (name, src) in sources {
        let path = work.join(name);
        std::fs::write(&path, src).ok()?;
        args.push(path.to_string_lossy().into_owned());
        for flag in language_directives::kotlinc_args(src) {
            if !language.contains(&flag) {
                language.push(flag);
            }
        }
    }
    args.extend(language);
    match kotlinc_compile(&args) {
        Some((0, _)) => Some(out),
        Some((code, err)) => panic!("kotlinc(lib) failed ({code}): {err}"),
        None => None,
    }
}
