//! `-Xjdk-release` against kotlinc. A build tool that pins the JDK passes the release of that same
//! JDK, and kotlinc then compiles against the JDK's own modules with the release as the JVM target.
//! krusty must emit the same classes. A release other than the JDK's own selects that release's
//! API from the JDK's `ct.sym`, which krusty refuses rather than compiling against the wrong API.

use std::path::{Path, PathBuf};

use super::common;

const SOURCE: &str =
    "package p\n\nclass Box(val value: String)\n\nfun box(): String = Box(\"OK\").value\n";

fn classes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("read compiler output") {
            let path = entry.expect("compiler output entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                let name = path
                    .strip_prefix(dir)
                    .expect("class below the output root")
                    .to_string_lossy()
                    .into_owned();
                found.push((name, std::fs::read(&path).expect("read class")));
            }
        }
    }
    found.sort();
    found
}

struct Fixture {
    work: PathBuf,
    jdk: String,
    release: u16,
}

impl Fixture {
    fn new() -> Self {
        let work = common::scratch_dir().expect("allocate a scratch directory");
        std::fs::write(work.join("Box.kt"), SOURCE).expect("write the fixture");
        let jdk = common::java_home();
        let release =
            krusty::jvm::compilation_inputs::selected_jdk_feature_release(Some(Path::new(&jdk)))
                .expect("the reference JDK names its release");
        Fixture { work, jdk, release }
    }

    fn arguments(&self, output: &str, extra: &[&str]) -> Vec<String> {
        let mut arguments = vec![
            self.work.join("Box.kt").to_string_lossy().into_owned(),
            "-jdk-home".into(),
            self.jdk.clone(),
            "-d".into(),
            self.work.join(output).to_string_lossy().into_owned(),
        ];
        arguments.extend(extra.iter().map(|argument| argument.to_string()));
        arguments
    }

    fn krusty(&self, output: &str, extra: &[&str]) -> std::process::Output {
        std::process::Command::new(common::krusty_binary())
            .args(self.arguments(output, extra))
            .output()
            .expect("run krusty")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

#[test]
fn the_jdks_own_release_compiles_as_kotlinc_does() {
    let fixture = Fixture::new();
    let release = format!("-Xjdk-release={}", fixture.release);
    let (code, stderr) =
        common::kotlinc_compile(&fixture.arguments("kotlinc", &[&release])).expect("kotlinc");
    assert_eq!((code, stderr.as_str()), (0, ""), "kotlinc {release}");

    let result = fixture.krusty("krusty", &[&release]);
    assert_eq!(result.status.code(), Some(0), "krusty {release}");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "",
        "krusty {release}"
    );
    let expected = classes(&fixture.work.join("kotlinc"));
    assert_eq!(expected.len(), 2, "kotlinc classes");
    assert_eq!(classes(&fixture.work.join("krusty")), expected, "{release}");
}

#[test]
fn a_conflicting_jvm_target_is_reported_in_kotlincs_words() {
    let fixture = Fixture::new();
    let release = format!("-Xjdk-release={}", fixture.release);
    let target = (fixture.release - 1).to_string();
    let extra = [release.as_str(), "-jvm-target", target.as_str()];
    let (code, stderr) =
        common::kotlinc_compile(&fixture.arguments("kotlinc", &extra)).expect("kotlinc");
    let message = format!(
        "'{release}' option conflicts with '-jvm-target {target}'. Please remove the '-jvm-target' option"
    );
    assert_eq!(
        (code, stderr.as_str()),
        (1, format!("error: {message}\n").as_str())
    );

    let output = fixture.work.join("krusty");
    let result = fixture.krusty("krusty", &extra);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        format!("krusty: error: {message}\n")
    );
    assert!(!output.exists(), "krusty wrote {}", output.display());
}

#[test]
fn another_release_is_refused_before_anything_is_written() {
    let fixture = Fixture::new();
    let older = fixture.release - 1;
    let release = format!("-Xjdk-release={older}");
    let output = fixture.work.join("krusty");
    let result = fixture.krusty("krusty", &[&release]);
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&result.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        format!(
            "krusty: error: {release} compiles against the JDK {older} API from ct.sym, which \
             krusty does not implement; the selected JDK is {}\n",
            fixture.release
        )
    );
    assert!(!output.exists(), "krusty wrote {}", output.display());
}
