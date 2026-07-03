//! Dependency provisioning — how krusty **acquires** a hosted plugin's toolchain (the KSP sidecar
//! jars: `symbol-processing-aa`/`-api` + `kotlin-compiler-embeddable` and their transitive closure).
//!
//! krusty is a compiler, not a build tool, so in production it does not vendor these jars. It either
//! (a) **detects an available resolver** on the host — Gradle, Maven, or Coursier — and drives it to
//! materialize an artifact + its full transitive closure into a folder, or (b) falls back to a
//! built-in minimal Maven fetcher (documented; not yet implemented) for hosts with no resolver. If
//! neither is available the host reports a clear error and the user supplies the jars via the same
//! `-Xplugin`/`apclasspath` switches (see `cli`).
//!
//! This keeps krusty faithful to its drop-in contract: the toolchain coordinate is the
//! build-resolved `KspToolchain` (tied to the kotlinc version); provisioning just turns that
//! coordinate into on-disk jars the sidecar runs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A host dependency resolver krusty can drive to download artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolver {
    /// `gradle` — driven via a generated throwaway project with a `Copy` task.
    Gradle(PathBuf),
    /// `mvn` — driven via `dependency:copy-dependencies` over a generated POM.
    Maven(PathBuf),
    /// `cs` (Coursier) — `cs fetch` resolves + prints the jar paths directly.
    Coursier(PathBuf),
}

/// Probe `PATH` for a usable resolver, preferring the lightest: **Coursier** (purpose-built jar
/// fetcher) → **Gradle** → **Maven**. Returns `None` if none is installed; the caller then reports
/// that the user must supply the jars (the built-in Maven fetcher fallback is documented in the
/// module header). The order is preference, not capability — all three resolve the same closure.
pub fn detect() -> Option<Resolver> {
    if let Some(p) = which("cs") {
        return Some(Resolver::Coursier(p));
    }
    if let Some(p) = which("gradle") {
        return Some(Resolver::Gradle(p));
    }
    if let Some(p) = which("mvn") {
        return Some(Resolver::Maven(p));
    }
    None
}

/// Locate an executable named `name` on `PATH` (first matching file). Detection order in [`detect`]
/// then prefers the lightest resolver.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

impl Resolver {
    /// Resolve `coords` (`group:artifact:version`) plus their transitive closure into `out_dir`,
    /// returning the jar paths placed there. Network + resolver are required.
    pub fn fetch(&self, coords: &[String], out_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        // Validate up front: a Maven coordinate is `group:artifact:version[:classifier]` over a
        // restricted charset. Rejecting here prevents a malformed coord from being silently dropped
        // (a missing KSP jar) AND closes script-injection via the generated gradle/pom text.
        for c in coords {
            if !is_valid_coord(c) {
                return Err(std::io::Error::other(format!(
                    "invalid Maven coordinate {c:?} (expected group:artifact:version)"
                )));
            }
        }
        std::fs::create_dir_all(out_dir)?;
        match self {
            Resolver::Gradle(bin) => self.fetch_gradle(bin, coords, out_dir),
            Resolver::Maven(bin) => self.fetch_maven(bin, coords, out_dir),
            Resolver::Coursier(bin) => self.fetch_coursier(bin, coords, out_dir),
        }
    }

    fn fetch_gradle(
        &self,
        bin: &Path,
        coords: &[String],
        out_dir: &Path,
    ) -> std::io::Result<Vec<PathBuf>> {
        let proj = out_dir.join(".gradle-fetch");
        std::fs::create_dir_all(&proj)?;
        std::fs::write(
            proj.join("settings.gradle.kts"),
            "rootProject.name = \"kspfetch\"\n",
        )?;
        std::fs::write(
            proj.join("build.gradle.kts"),
            gradle_build_script(coords, out_dir),
        )?;
        let status = Command::new(bin)
            .current_dir(&proj)
            .args(["--no-daemon", "-q", "fetchJars"])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other("gradle fetchJars failed"));
        }
        let _ = std::fs::remove_dir_all(&proj);
        collect_jars(out_dir)
    }

    fn fetch_maven(
        &self,
        bin: &Path,
        coords: &[String],
        out_dir: &Path,
    ) -> std::io::Result<Vec<PathBuf>> {
        let pom = out_dir.join("pom.xml");
        std::fs::write(&pom, maven_pom(coords))?;
        let status = Command::new(bin)
            .args(["-q", "-f"])
            .arg(&pom)
            .arg("dependency:copy-dependencies")
            .arg(format!("-DoutputDirectory={}", out_dir.display()))
            .status()?;
        let _ = std::fs::remove_file(&pom);
        if !status.success() {
            return Err(std::io::Error::other(
                "mvn dependency:copy-dependencies failed",
            ));
        }
        collect_jars(out_dir)
    }

    fn fetch_coursier(
        &self,
        bin: &Path,
        coords: &[String],
        out_dir: &Path,
    ) -> std::io::Result<Vec<PathBuf>> {
        // `cs fetch` prints one resolved jar path per line (from its cache); copy each into out_dir.
        let out = Command::new(bin).arg("fetch").args(coords).output()?;
        if !out.status.success() {
            return Err(std::io::Error::other("cs fetch failed"));
        }
        let mut jars = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let src = Path::new(line.trim());
            if src.extension().is_some_and(|e| e == "jar") && src.is_file() {
                let dst = out_dir.join(src.file_name().unwrap());
                std::fs::copy(src, &dst)?;
                jars.push(dst);
            }
        }
        Ok(jars)
    }
}

/// List the jars directly in `dir` (the materialized closure).
fn collect_jars(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut jars: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jar"))
        .collect();
    jars.sort();
    Ok(jars)
}

/// A Maven coordinate krusty will interpolate into a build script: `group:artifact:version` with an
/// optional `:classifier`, over `[A-Za-z0-9._-]`. Anything else is rejected (see `fetch`).
pub fn is_valid_coord(c: &str) -> bool {
    let parts: Vec<&str> = c.split(':').collect();
    (3..=4).contains(&parts.len())
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        })
}

/// The Gradle build that resolves `coords` + transitive deps and copies the closure into `out_dir`.
/// Pure (no I/O) so it is unit-testable. `coords` are pre-validated by [`is_valid_coord`]; the path is
/// escaped for the Kotlin string literal it lands in.
pub fn gradle_build_script(coords: &[String], out_dir: &Path) -> String {
    let deps = coords
        .iter()
        .map(|c| format!("    fetch(\"{c}\")\n"))
        .collect::<String>();
    let out = out_dir
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "plugins {{ base }}\n\
         repositories {{ mavenCentral() }}\n\
         val fetch by configurations.creating\n\
         dependencies {{\n{deps}}}\n\
         tasks.register<Copy>(\"fetchJars\") {{\n\
         \x20   from(configurations[\"fetch\"])\n\
         \x20   into(\"{out}\")\n\
         }}\n"
    )
}

/// A minimal Maven POM declaring `coords` as dependencies, for `dependency:copy-dependencies`.
/// Pure (no I/O) so it is unit-testable.
pub fn maven_pom(coords: &[String]) -> String {
    let deps = coords
        .iter()
        .filter_map(|c| {
            let mut it = c.split(':');
            Some(format!(
                "    <dependency><groupId>{}</groupId><artifactId>{}</artifactId><version>{}</version></dependency>\n",
                it.next()?, it.next()?, it.next()?
            ))
        })
        .collect::<String>();
    format!(
        "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n\
         <modelVersion>4.0.0</modelVersion>\n\
         <groupId>org.krusty</groupId><artifactId>ksp-fetch</artifactId><version>0</version>\n\
         <dependencies>\n{deps}</dependencies>\n</project>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coords() -> Vec<String> {
        vec![
            "com.google.devtools.ksp:symbol-processing-api:2.0.21-1.0.28".to_string(),
            "org.jetbrains.kotlin:kotlin-compiler-embeddable:2.0.21".to_string(),
        ]
    }

    #[test]
    fn gradle_script_lists_all_coords_and_copies_to_out() {
        let script = gradle_build_script(&coords(), Path::new("/tmp/ksp-out"));
        assert!(script.contains("mavenCentral()"));
        assert!(script.contains("symbol-processing-api:2.0.21-1.0.28"));
        assert!(script.contains("kotlin-compiler-embeddable:2.0.21"));
        assert!(script.contains("Copy"));
        assert!(script.contains("/tmp/ksp-out"));
    }

    #[test]
    fn maven_pom_declares_each_dependency() {
        let pom = maven_pom(&coords());
        assert!(pom.contains("<artifactId>symbol-processing-api</artifactId>"));
        assert!(pom.contains("<version>2.0.21-1.0.28</version>"));
        assert!(pom.contains("<groupId>org.jetbrains.kotlin</groupId>"));
        // A malformed coord (missing version) is skipped, not panicked on.
        let pom2 = maven_pom(&["only:two".to_string()]);
        assert!(!pom2.contains("only"));
    }

    #[test]
    fn coord_validation_rejects_malformed_and_injection() {
        assert!(is_valid_coord(
            "com.google.devtools.ksp:symbol-processing-api:2.0.21-1.0.28"
        ));
        assert!(is_valid_coord("g:a:1.0:linux")); // classifier ok
        assert!(!is_valid_coord("only:two")); // missing version
        assert!(!is_valid_coord("g::1.0")); // empty artifact
                                            // Injection attempts are rejected (quotes / gradle syntax / whitespace).
        assert!(!is_valid_coord("g:a:1\") ; exec(\"rm -rf /\")"));
        assert!(!is_valid_coord("g:a:1 2"));
    }

    #[test]
    fn fetch_errors_on_bad_coord_without_touching_network() {
        // A malformed coord fails fast (no resolver/network needed) rather than silently dropping it.
        let r = Resolver::Gradle(std::path::PathBuf::from("/nonexistent/gradle"));
        let err = r
            .fetch(&["bad-coord".to_string()], Path::new("/tmp/krusty-x"))
            .unwrap_err();
        assert!(err.to_string().contains("invalid Maven coordinate"));
    }

    #[test]
    fn detect_does_not_panic() {
        // Environment-dependent; just exercise the probe.
        let _ = detect();
    }
}
