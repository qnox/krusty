//! JVM the project daemon uses to compile the injected init script.
//!
//! The Tooling API client runs on the JVM that launched it. Gradle starts the project daemon on
//! that JVM unless the build names another. Gradle 8.0 cannot read Java 21 class files (major
//! version 65), so compiling the init script fails inside the daemon. When that JVM is outside the
//! range the project's Gradle can run, the build action selects an installed JDK the release can
//! run and passes it to `BuildLauncher.setJavaHome`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Where to look for the daemon JVM. Tests supply the homes; production reads the machine.
#[derive(Clone, Debug)]
pub(super) struct JvmSearch {
    /// JVM the daemon would use before this selection.
    pub(super) daemon_home: Option<PathBuf>,
    /// `org.gradle.java.home` after Gradle's precedence. Empty when the build did not set one.
    pub(super) explicit_home: Option<PathBuf>,
    /// Directories whose children are JVM installations.
    pub(super) install_roots: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GradleVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl GradleVersion {
    const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

/// First Gradle release that can *run* on this Java major version.
///
/// Gradle's compatibility table lists toolchain support separately. Gradle 8.4 can compile for
/// Java 21, and running Gradle itself on Java 21 starts at 8.5. A release that is not in this
/// table cannot be selected.
const RUNS_ON_JAVA: &[(u32, GradleVersion)] = &[
    (8, GradleVersion::new(2, 0, 0)),
    (9, GradleVersion::new(4, 3, 0)),
    (10, GradleVersion::new(4, 7, 0)),
    (11, GradleVersion::new(5, 0, 0)),
    (12, GradleVersion::new(5, 4, 0)),
    (13, GradleVersion::new(6, 0, 0)),
    (14, GradleVersion::new(6, 3, 0)),
    (15, GradleVersion::new(6, 7, 0)),
    (16, GradleVersion::new(7, 0, 0)),
    (17, GradleVersion::new(7, 3, 0)),
    (18, GradleVersion::new(7, 5, 0)),
    (19, GradleVersion::new(7, 6, 0)),
    (20, GradleVersion::new(8, 3, 0)),
    (21, GradleVersion::new(8, 5, 0)),
    (22, GradleVersion::new(8, 8, 0)),
    (23, GradleVersion::new(8, 10, 0)),
    (24, GradleVersion::new(8, 14, 0)),
    (25, GradleVersion::new(9, 1, 0)),
    (26, GradleVersion::new(9, 4, 0)),
    (27, GradleVersion::new(9, 8, 0)),
];

impl JvmSearch {
    pub(super) fn system(project: &Path) -> Self {
        let user = super::tooling::gradle_user_home();
        Self::for_project(project, client_java_home(), install_roots(), Some(&user))
    }

    fn for_project(
        project: &Path,
        client_home: Option<PathBuf>,
        install_roots: Vec<PathBuf>,
        user_gradle_home: Option<&Path>,
    ) -> Self {
        let explicit_home = explicit_java_home(project, user_gradle_home);
        Self {
            daemon_home: explicit_home.clone().or(client_home),
            explicit_home,
            install_roots,
        }
    }
}

/// The JVM passed to `BuildLauncher.setJavaHome`.
///
/// A compatible `org.gradle.java.home` is kept. The user Gradle home wins over the project file, so
/// an incompatible project value cannot replace a user JVM the release can run. `None` means the
/// client JVM can already run this Gradle and the build did not name another.
pub(super) fn daemon_java_home(project: &Path, search: &JvmSearch) -> Option<PathBuf> {
    let version = gradle_version(project)?;
    if let Some(home) = search.explicit_home.as_deref() {
        if jvm_major(home).is_some_and(|major| runs_on(version, major)) {
            return Some(home.to_path_buf());
        }
    }
    let current = search.daemon_home.as_deref().and_then(jvm_major)?;
    if runs_on(version, current) {
        return None;
    }
    newest_runnable(version, search)
}

fn runs_on(gradle: GradleVersion, java: u32) -> bool {
    let Some(first) = RUNS_ON_JAVA
        .iter()
        .find(|(major, _)| *major == java)
        .map(|(_, first)| *first)
    else {
        return false;
    };
    java >= minimum_java(gradle) && gradle >= first
}

fn minimum_java(version: GradleVersion) -> u32 {
    if version >= GradleVersion::new(9, 0, 0) {
        17
    } else {
        8
    }
}

fn newest_runnable(version: GradleVersion, search: &JvmSearch) -> Option<PathBuf> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for root in &search.install_roots {
        collect_homes(root, &mut found, &mut seen);
    }
    found.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    found
        .into_iter()
        .find_map(|(major, home)| runs_on(version, major).then_some(home))
}

fn collect_homes(root: &Path, found: &mut Vec<(u32, PathBuf)>, seen: &mut HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        consider_home(&path, found, seen);
        consider_home(&path.join("Contents").join("Home"), found, seen);
    }
}

fn consider_home(home: &Path, found: &mut Vec<(u32, PathBuf)>, seen: &mut HashSet<PathBuf>) {
    let Some(major) = jvm_major(home) else {
        return;
    };
    let key = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if seen.insert(key) {
        found.push((major, home.to_path_buf()));
    }
}

fn jvm_major(home: &Path) -> Option<u32> {
    if !java_binary(home).is_file() {
        return None;
    }
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    java_major_from_release(&text)
}

fn java_binary(home: &Path) -> PathBuf {
    home.join("bin").join(java_binary_name())
}

fn java_binary_name() -> &'static str {
    if cfg!(windows) {
        "java.exe"
    } else {
        "java"
    }
}

fn java_major_from_release(text: &str) -> Option<u32> {
    for line in text.lines() {
        let Some(value) = line.trim().strip_prefix("JAVA_VERSION=") else {
            continue;
        };
        return java_major(value.trim().trim_matches('"'));
    }
    None
}

fn java_major(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '+', '-', '_']);
    let first = parts.next()?.parse::<u32>().ok()?;
    if first == 1 {
        return parts.next()?.parse().ok();
    }
    Some(first)
}

fn gradle_version(project: &Path) -> Option<GradleVersion> {
    let text =
        std::fs::read_to_string(project.join("gradle/wrapper/gradle-wrapper.properties")).ok()?;
    version_from_wrapper(&text)
}

fn version_from_wrapper(text: &str) -> Option<GradleVersion> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "distributionUrl" {
            return version_in_distribution_url(value.trim());
        }
    }
    None
}

fn version_in_distribution_url(url: &str) -> Option<GradleVersion> {
    let marker = "gradle-";
    let start = url.rfind(marker)? + marker.len();
    let mut parts = [0u32; 3];
    let mut index = 0usize;
    let mut number: Option<u32> = None;
    for byte in url[start..].bytes() {
        if byte.is_ascii_digit() {
            let slot = number.get_or_insert(0);
            *slot = slot
                .saturating_mul(10)
                .saturating_add(u32::from(byte - b'0'));
        } else if byte == b'.' && index < 2 {
            parts[index] = number.take()?;
            index += 1;
        } else {
            break;
        }
    }
    parts[index] = number?;
    Some(GradleVersion::new(parts[0], parts[1], parts[2]))
}

fn explicit_java_home(project: &Path, user_gradle_home: Option<&Path>) -> Option<PathBuf> {
    user_gradle_home
        .and_then(|home| property_path(&home.join("gradle.properties"), "org.gradle.java.home"))
        .or_else(|| property_path(&project.join("gradle.properties"), "org.gradle.java.home"))
}

fn property_path(file: &Path, key: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    Some(PathBuf::from(property_value(&text, key)?))
}

fn property_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// The JVM `java_bin` launches: `JAVA_HOME` when it contains a `java` executable, otherwise the
/// `java` on `PATH`.
fn client_java_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let home = PathBuf::from(home);
        if java_binary(&home).is_file() {
            return Some(home);
        }
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let executable = directory.join(java_binary_name());
        if !executable.is_file() {
            continue;
        }
        let resolved = std::fs::canonicalize(&executable).unwrap_or(executable);
        return resolved
            .parent()
            .and_then(|bin| bin.parent())
            .map(Path::to_path_buf);
    }
    None
}

fn install_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if cfg!(windows) {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            let program_files = PathBuf::from(program_files);
            roots.push(program_files.join("Java"));
            roots.push(program_files.join("Eclipse Adoptium"));
            roots.push(program_files.join("Microsoft"));
        }
    } else {
        roots.push(PathBuf::from("/usr/lib/jvm"));
        roots.push(PathBuf::from("/usr/lib64/jvm"));
        roots.push(PathBuf::from("/usr/java"));
        if cfg!(target_os = "macos") {
            roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::testing::TempTree;

    fn write_jvm(tree: &TempTree, relative: &str, version: &str) -> PathBuf {
        let home = tree.directory(relative);
        tree.write(
            &format!("{relative}/release"),
            &format!("JAVA_VERSION=\"{version}\"\n"),
        );
        tree.write(&format!("{relative}/bin/java"), "");
        home
    }

    fn wrapper(version: &str) -> String {
        format!(
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-{version}-bin.zip\n"
        )
    }

    fn homes(tree: &TempTree, current: &Path) -> JvmSearch {
        JvmSearch {
            daemon_home: Some(current.to_path_buf()),
            explicit_home: None,
            install_roots: vec![tree.path("jvms")],
        }
    }

    #[test]
    fn daemon_jvm_keeps_a_client_jvm_the_release_can_run() {
        let tree = TempTree::new("daemon-jvm-keep");
        tree.write("gradle/wrapper/gradle-wrapper.properties", &wrapper("8.5"));
        let java_21 = write_jvm(&tree, "jvms/21", "21.0.2");
        write_jvm(&tree, "jvms/17", "17.0.13");
        assert_eq!(daemon_java_home(tree.root(), &homes(&tree, &java_21)), None);
    }

    #[test]
    fn daemon_jvm_picks_the_newest_java_the_release_can_run() {
        let tree = TempTree::new("daemon-jvm-newest");
        tree.write("gradle/wrapper/gradle-wrapper.properties", &wrapper("8.0"));
        write_jvm(&tree, "jvms/11", "11.0.25");
        write_jvm(&tree, "jvms/17", "17.0.13");
        let java_19 = write_jvm(&tree, "jvms/19", "19.0.2");
        let java_21 = write_jvm(&tree, "jvms/21", "21.0.2");
        assert_eq!(
            daemon_java_home(tree.root(), &homes(&tree, &java_21)).as_deref(),
            Some(java_19.as_path())
        );
    }

    #[test]
    fn daemon_jvm_compares_gradle_versions_numerically() {
        let tree = TempTree::new("daemon-jvm-order");
        let java_21 = write_jvm(&tree, "jvms/21", "21.0.2");
        let java_17 = write_jvm(&tree, "jvms/17", "17.0.13");
        let search = homes(&tree, &java_21);
        tree.write("gradle/wrapper/gradle-wrapper.properties", &wrapper("8.10"));
        assert_eq!(daemon_java_home(tree.root(), &search), None);
        tree.write("gradle/wrapper/gradle-wrapper.properties", &wrapper("8.4"));
        assert_eq!(
            daemon_java_home(tree.root(), &search).as_deref(),
            Some(java_17.as_path())
        );
    }

    #[test]
    fn daemon_jvm_keeps_a_compatible_org_gradle_java_home() {
        let tree = TempTree::new("daemon-jvm-property");
        tree.write("gradle/wrapper/gradle-wrapper.properties", &wrapper("8.0"));
        let java_17 = write_jvm(&tree, "jvms/17", "17.0.13");
        let java_21 = write_jvm(&tree, "jvms/21", "21.0.2");
        tree.write(
            "gradle.properties",
            &format!("org.gradle.java.home={}\n", java_17.display()),
        );
        let search =
            JvmSearch::for_project(tree.root(), Some(java_21), vec![tree.path("jvms")], None);
        assert_eq!(
            daemon_java_home(tree.root(), &search).as_deref(),
            Some(java_17.as_path())
        );
    }

    #[test]
    fn daemon_jvm_prefers_the_user_java_home_over_an_incompatible_project_file() {
        let tree = TempTree::new("daemon-jvm-user-home");
        tree.write("gradle/wrapper/gradle-wrapper.properties", &wrapper("8.0"));
        let project_java = write_jvm(&tree, "jvms/21", "21.0.2");
        let installed = write_jvm(&tree, "jvms/19", "19.0.2");
        let user_java = write_jvm(&tree, "outside/17", "17.0.13");
        tree.write(
            "gradle.properties",
            &format!("org.gradle.java.home={}\n", project_java.display()),
        );
        let user_gradle = tree.directory("gradle-user");
        tree.write(
            "gradle-user/gradle.properties",
            &format!("org.gradle.java.home={}\n", user_java.display()),
        );
        let search = JvmSearch::for_project(
            tree.root(),
            Some(project_java),
            vec![tree.path("jvms")],
            Some(&user_gradle),
        );
        let chosen = daemon_java_home(tree.root(), &search);
        assert_eq!(chosen.as_deref(), Some(user_java.as_path()));
        assert_ne!(chosen.as_deref(), Some(installed.as_path()));
    }

    #[test]
    fn daemon_jvm_does_not_run_gradle_9_on_java_11() {
        let tree = TempTree::new("daemon-jvm-gradle9");
        tree.write("gradle/wrapper/gradle-wrapper.properties", &wrapper("9.0"));
        let java_11 = write_jvm(&tree, "jvms/11", "11.0.25");
        let java_21 = write_jvm(&tree, "jvms/21", "21.0.2");
        assert_eq!(
            daemon_java_home(tree.root(), &homes(&tree, &java_11)).as_deref(),
            Some(java_21.as_path())
        );
    }

    #[test]
    fn a_java_8_release_file_reports_major_8() {
        assert_eq!(java_major("1.8.0_402"), Some(8));
        assert_eq!(java_major("21.0.13.0.3"), Some(21));
    }
}
