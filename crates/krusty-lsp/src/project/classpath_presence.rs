//! Whether the classpath a project model resolved is actually present on disk.
//!
//! Build models list library jars and module outputs whether or not they were ever downloaded or
//! compiled: a JPS project IntelliJ never synced references `$MAVEN_REPOSITORY$` jars that are
//! absent, and its `out/production` directories do not exist until the first build. Analysis skips
//! such entries, so every reference into them, the Kotlin standard library included, is reported as
//! unresolved. The server says so once, with examples, instead of leaving thousands of unexplained
//! errors.

use std::path::{Path, PathBuf};

const MAX_EXAMPLES: usize = 3;

/// A warning naming how many `classpath` entries `exists` rejects, or `None` when all are present.
///
/// Missing jars are listed before missing directories: an absent library is the likelier root
/// cause, and module outputs only appear after a build that itself needs those libraries.
pub fn missing_classpath_warning(
    classpath: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<String> {
    let missing = classpath
        .iter()
        .filter(|entry| !exists(entry))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return None;
    }
    let is_jar = |entry: &&&PathBuf| {
        entry
            .extension()
            .is_some_and(|extension| extension == "jar")
    };
    let examples = missing
        .iter()
        .filter(is_jar)
        .chain(missing.iter().filter(|entry| !is_jar(entry)))
        .take(MAX_EXAMPLES)
        .map(|entry| entry.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "krusty: {} of {} classpath entries do not exist (e.g. {examples}); references into them \
         are unresolved. Download the project's libraries and build it, then reload.",
        missing.len(),
        classpath.len(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_classpath_entries_are_reported_jars_first() {
        let classpath = [
            "/ws/out/production/core",
            "/ws/out/production/util.zip",
            "/m2/kotlin-stdlib.jar",
            "/m2/present.jar",
            "/m2/annotations.jar",
        ]
        .map(PathBuf::from);
        let warning =
            missing_classpath_warning(&classpath, |entry| entry == Path::new("/m2/present.jar"));
        assert_eq!(
            warning.as_deref(),
            Some(
                "krusty: 4 of 5 classpath entries do not exist (e.g. /m2/kotlin-stdlib.jar, \
                 /m2/annotations.jar, /ws/out/production/core); references into them are \
                 unresolved. Download the project's libraries and build it, then reload."
            )
        );
    }

    #[test]
    fn a_present_classpath_has_no_warning() {
        let classpath = ["/m2/kotlin-stdlib.jar"].map(PathBuf::from);
        assert_eq!(missing_classpath_warning(&classpath, |_| true), None);
        assert_eq!(missing_classpath_warning(&[], |_| false), None);
    }
}
