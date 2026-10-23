//! Differential fixtures for kotlinc compiler plugins krusty reimplements natively: each fixture
//! compiles the same sources with the same `-Xplugin`/`-P` switches through kotlinc and through the
//! krusty binary, then requires the same class files byte for byte and `box()` returning `OK`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::common;

/// The jar of the reference distribution's compiler plugin called `name`
/// (`allopen-compiler-plugin.jar`, …).
pub fn kotlinc_plugin_jar(name: &str) -> PathBuf {
    let jar = common::kotlinc_lib_dir()
        .expect("the reference kotlinc distribution is provisioned")
        .join(name);
    assert!(
        jar.is_file(),
        "the reference distribution ships {}",
        jar.display()
    );
    jar
}

/// `-Xplugin=<jar>` followed by `-P plugin:<id>:<option>` for each option.
pub fn plugin_switches(jar: &str, plugin_id: &str, options: &[&str]) -> Vec<String> {
    let mut switches = vec![format!("-Xplugin={}", kotlinc_plugin_jar(jar).display())];
    for option in options {
        switches.extend(["-P".to_string(), format!("plugin:{plugin_id}:{option}")]);
    }
    switches
}

fn join_classpath(entries: &[PathBuf]) -> String {
    std::env::join_paths(entries)
        .expect("classpath entries contain no separator")
        .to_string_lossy()
        .into_owned()
}

/// The `.class` files under `dir`, keyed by their path relative to it, in path order.
fn classes_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.map(|entry| entry.expect("readable output entry").path()) {
            if entry.is_dir() {
                walk(base, &entry, out);
            } else if entry
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                let relative = entry
                    .strip_prefix(base)
                    .expect("entry under the output")
                    .with_extension("");
                let name = relative.to_string_lossy().replace('\\', "/");
                out.push((name, std::fs::read(&entry).expect("read the class file")));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

/// One compilation, built by kotlinc and by the krusty binary from the same sources and switches.
pub struct PluginFixture {
    dir: PathBuf,
}

impl PluginFixture {
    /// A fresh scratch directory for the fixture called `name`.
    pub fn new(name: &str) -> PluginFixture {
        let dir = common::scratch_dir()
            .expect("a scratch directory")
            .join(format!("compiler-plugin-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the fixture directory");
        PluginFixture { dir }
    }

    fn write(&self, sources: &[(&str, &str)], unit: &str) -> Vec<String> {
        let source_dir = self.dir.join(unit).join("src");
        std::fs::create_dir_all(&source_dir).expect("create the source directory");
        sources
            .iter()
            .map(|(name, text)| {
                let path = source_dir.join(name);
                std::fs::write(&path, text).expect("write a fixture source");
                path.display().to_string()
            })
            .collect()
    }

    /// Build `sources` with kotlinc into `<unit>/kotlinc`.
    pub fn kotlinc(
        &self,
        unit: &str,
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        switches: &[String],
    ) -> PathBuf {
        let files = self.write(sources, unit);
        let out = self.dir.join(unit).join("kotlinc");
        let mut arguments = switches.to_vec();
        if !classpath.is_empty() {
            arguments.extend(["-cp".to_string(), join_classpath(classpath)]);
        }
        arguments.extend(["-d".to_string(), out.display().to_string()]);
        arguments.extend(files);
        let (code, diagnostics) =
            common::kotlinc_compile(&arguments).expect("the reference compiler runs");
        assert_eq!(code, 0, "kotlinc rejected {unit}: {diagnostics}");
        out
    }

    /// Build `sources` with kotlinc, which must reject them; its diagnostics, with the fixture
    /// directory spelled `<fixture>`.
    pub fn kotlinc_rejects(
        &self,
        unit: &str,
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        switches: &[String],
    ) -> String {
        let files = self.write(sources, unit);
        let mut arguments = switches.to_vec();
        if !classpath.is_empty() {
            arguments.extend(["-cp".to_string(), join_classpath(classpath)]);
        }
        let out = self.dir.join(unit).join("kotlinc");
        arguments.extend(["-d".to_string(), out.display().to_string()]);
        arguments.extend(files);
        let (code, diagnostics) =
            common::kotlinc_compile(&arguments).expect("the reference compiler runs");
        assert_ne!(code, 0, "kotlinc must reject {unit}");
        self.normalized(&diagnostics)
    }

    /// `diagnostics` without `info:` notes, the fixture directory spelled `<fixture>` whether a
    /// compiler printed it absolute or relative to the working directory.
    fn normalized(&self, diagnostics: &str) -> String {
        let absolute = self.dir.to_string_lossy().into_owned();
        let relative = std::env::current_dir()
            .ok()
            .and_then(|cwd| self.dir.strip_prefix(cwd).ok().map(Path::to_path_buf))
            .map(|dir| dir.to_string_lossy().into_owned());
        diagnostics
            .lines()
            .filter(|line| !line.starts_with("info: "))
            .map(|line| {
                let line = line.replace(&absolute, "<fixture>");
                let line = match &relative {
                    Some(relative) => line.replace(relative, "<fixture>"),
                    None => line,
                };
                line + "\n"
            })
            .collect()
    }

    /// Build `sources` with the krusty binary into `<unit>/krusty`.
    pub fn krusty(
        &self,
        unit: &str,
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        switches: &[String],
    ) -> PathBuf {
        let (code, diagnostics) = self.run_krusty(unit, sources, classpath, switches);
        assert_eq!(code, Some(0), "krusty rejected {unit}: {diagnostics}");
        self.dir.join(unit).join("krusty")
    }

    /// Build `sources` with the krusty binary, which must reject them; its diagnostics, with the
    /// fixture directory spelled `<fixture>` and without the `info:` notes it prints first.
    pub fn krusty_rejects(
        &self,
        unit: &str,
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        switches: &[String],
    ) -> String {
        let (code, diagnostics) = self.run_krusty(unit, sources, classpath, switches);
        assert_eq!(code, Some(1), "krusty must reject {unit}: {diagnostics}");
        self.normalized(&diagnostics)
    }

    fn run_krusty(
        &self,
        unit: &str,
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        switches: &[String],
    ) -> (Option<i32>, String) {
        let files = self.write(sources, unit);
        let out = self.dir.join(unit).join("krusty");
        let mut command = Command::new(common::krusty_binary());
        if !classpath.is_empty() {
            command.args(["-cp", &join_classpath(classpath)]);
        }
        let result = command
            .args(switches)
            .arg("-d")
            .arg(&out)
            .args(files)
            .output()
            .expect("run krusty");
        (
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).into_owned(),
        )
    }
}

/// Every class kotlinc emitted, and no other, with identical bytes; then `box()` on krusty's output.
pub fn assert_same_classes_and_box(reference: &Path, krusty: &Path, runtime: &[PathBuf]) {
    let actual = assert_same_classes(reference, krusty);
    let box_class = common::find_box_class(&actual).expect("a class declaring box()");
    assert_eq!(
        common::run_box(&actual, &box_class, runtime).as_deref(),
        Some("OK")
    );
}

/// Every class kotlinc emitted, and no other, with identical bytes; krusty's classes.
pub fn assert_same_classes(reference: &Path, krusty: &Path) -> Vec<(String, Vec<u8>)> {
    let expected = classes_in(reference);
    let actual = classes_in(krusty);
    assert_eq!(
        actual.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        expected.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "krusty emitted a different class set"
    );
    for ((name, actual), (_, expected)) in actual.iter().zip(&expected) {
        assert!(actual == expected, "{name}.class differs from kotlinc's");
    }
    actual
}
