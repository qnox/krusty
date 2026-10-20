//! The JDK a module compiles against (`jvm.jdk`): the one `JAVA_HOME` names, when it is a JDK
//! (it has `bin/javac`) whose major version is the one the module asks for. The toolchain provisions a JDK otherwise, and
//! krusty-toolchain refuses instead.

use std::path::{Path, PathBuf};

use super::refusals::at_setting;
use crate::configuration::Configuration;
use crate::diagnostic::Diagnostic;
use crate::tree::{Node, Value};

/// The JDK home `module` compiles against, given its main fragment's `settings` and the host's
/// `JAVA_HOME`, or why krusty-toolchain does not select one.
pub(super) fn select(
    module: &str,
    configuration: &Configuration,
    settings: &Node,
    java_home: Option<&Path>,
) -> Result<PathBuf, Diagnostic> {
    let jdk = settings.at(&["jvm", "jdk"]);
    let setting = |name: &str| jdk.and_then(|jdk| jdk.get(name));
    let refuse = |node: Option<&Node>, message: String| match node {
        Some(node) => at_setting(configuration, node, message),
        None => Diagnostic::project_error(message),
    };
    if let Some(mode) = setting("selectionMode") {
        if matches!(mode.value, Value::Enum(_, "alwaysProvision")) {
            return Err(refuse(
                Some(mode),
                format!(
                    "module `{module}` sets `settings.jvm.jdk.selectionMode: alwaysProvision`, and krusty-toolchain does not provision JDKs"
                ),
            ));
        }
    }
    if let Some(distributions) = setting("distributions") {
        if !matches!(distributions.value, Value::Null) {
            return Err(refuse(
                Some(distributions),
                format!(
                    "module `{module}` sets `settings.jvm.jdk.distributions`, and krusty-toolchain does not check a JDK's distribution"
                ),
            ));
        }
    }
    let version = setting("version");
    let Some(Value::Int(wanted)) = version.map(|version| &version.value) else {
        return Err(refuse(
            version,
            format!("module `{module}` sets no JDK version (`settings.jvm.jdk.version`)"),
        ));
    };
    let Some(home) = java_home else {
        return Err(refuse(
            version,
            format!(
                "module `{module}` compiles against JDK {wanted}, but JAVA_HOME is not set, and krusty-toolchain does not provision JDKs"
            ),
        ));
    };
    let javac = if cfg!(windows) { "javac.exe" } else { "javac" };
    if !home.join("bin").join(javac).is_file() {
        return Err(refuse(
            version,
            format!(
                "module `{module}` compiles against JDK {wanted}, but JAVA_HOME ({}) is not a JDK: it has no `bin/{javac}`, and krusty-toolchain does not provision JDKs",
                home.display()
            ),
        ));
    }
    match major_version(home) {
        Some(found) if found == *wanted => Ok(home.to_path_buf()),
        Some(found) => Err(refuse(
            version,
            format!(
                "module `{module}` compiles against JDK {wanted}, but JAVA_HOME ({}) is JDK {found}, and krusty-toolchain does not provision JDKs",
                home.display()
            ),
        )),
        None => Err(refuse(
            version,
            format!(
                "module `{module}` compiles against JDK {wanted}, but JAVA_HOME ({}) has no `release` file naming its `JAVA_VERSION`",
                home.display()
            ),
        )),
    }
}

/// The major version of the JDK at `home`, from the `JAVA_VERSION` of its `release` file:
/// `25` for `"25.0.2"`, `8` for `"1.8.0_472"`.
fn major_version(home: &Path) -> Option<i32> {
    let release = std::fs::read_to_string(home.join("release")).ok()?;
    let version = release.lines().find_map(|line| {
        line.strip_prefix("JAVA_VERSION=")
            .map(|value| value.trim().trim_matches('"').to_string())
    })?;
    let mut parts = version.split(['.', '_', '-', '+']);
    let first = parts.next()?;
    let major = if first == "1" { parts.next()? } else { first };
    major.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::major_version;

    #[test]
    fn the_major_version_is_read_from_the_release_file() {
        let home = std::env::temp_dir().join(format!("krusty-jdk-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        for (release, major) in [
            ("JAVA_VERSION=\"25.0.2\"\n", Some(25)),
            ("IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"1.8.0_472\"\n", Some(8)),
            ("JAVA_VERSION=\"26-ea\"\n", Some(26)),
            ("JAVA_RUNTIME_VERSION=\"21\"\n", None),
        ] {
            std::fs::write(home.join("release"), release).unwrap();
            assert_eq!(major_version(&home), major, "{release}");
        }
        std::fs::remove_file(home.join("release")).unwrap();
        assert_eq!(major_version(&home), None);
        let _ = std::fs::remove_dir_all(&home);
    }
}
