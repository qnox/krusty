//! `-Xjdk-release` against kotlinc. A build tool that pins the JDK passes the release of that same
//! JDK, and kotlinc then compiles against the JDK's own modules with the release as the JVM target.
//! A release other than the JDK's own selects that release's API from the JDK's `lib/ct.sym`: the
//! classes then call the JDK members as that release declares them. krusty must emit the same
//! classes, and report a release the JDK cannot provide in kotlinc's words.

use std::path::{Path, PathBuf};

use super::common;

const SOURCE: &str =
    "package p\n\nclass Box(val value: String)\n\nfun box(): String = Box(\"OK\").value\n";

/// `ByteBuffer.flip()` returns `Buffer` in JDK 8 and is overridden to return `ByteBuffer` from
/// JDK 9 on, so the call's descriptor names the API the compilation read.
const FLIP: &str = "package p\n\nimport java.nio.ByteBuffer\n\n\
fun flip(buffer: ByteBuffer) {\n    buffer.flip()\n}\n\n\
fun box(): String {\n    flip(ByteBuffer.allocate(4))\n    return \"OK\"\n}\n";

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
        Self::with_source(SOURCE)
    }

    fn with_source(source: &str) -> Self {
        let work = common::scratch_dir().expect("allocate a scratch directory");
        std::fs::write(work.join("Box.kt"), source).expect("write the fixture");
        let jdk = common::java_home();
        let release =
            krusty::jvm::compilation_inputs::selected_jdk_feature_release(Some(Path::new(&jdk)))
                .expect("the reference JDK names its release");
        Fixture { work, jdk, release }
    }

    fn arguments(&self, output: &str, extra: &[&str]) -> Vec<String> {
        self.arguments_with_jdk(&self.jdk, output, extra)
    }

    fn arguments_with_jdk(&self, jdk: &str, output: &str, extra: &[&str]) -> Vec<String> {
        let mut arguments = vec![
            self.work.join("Box.kt").to_string_lossy().into_owned(),
            "-jdk-home".into(),
            jdk.to_string(),
            "-d".into(),
            self.work.join(output).to_string_lossy().into_owned(),
        ];
        arguments.extend(extra.iter().map(|argument| argument.to_string()));
        arguments
    }

    fn krusty(&self, output: &str, extra: &[&str]) -> std::process::Output {
        self.krusty_with_jdk(&self.jdk, output, extra)
    }

    fn krusty_with_jdk(&self, jdk: &str, output: &str, extra: &[&str]) -> std::process::Output {
        std::process::Command::new(common::krusty_binary())
            .args(self.arguments_with_jdk(jdk, output, extra))
            .output()
            .expect("run krusty")
    }

    /// kotlinc's errors for `extra` under the JDK at `jdk`, then krusty's, which must be the same
    /// errors in the same order, with nothing written.
    fn assert_rejected_as_kotlinc_does(&self, jdk: &str, extra: &[&str], expected: &[String]) {
        let (code, stderr) =
            common::kotlinc_compile(&self.arguments_with_jdk(jdk, "kotlinc", extra))
                .expect("kotlinc");
        let kotlinc = expected
            .iter()
            .map(|error| format!("error: {error}\n"))
            .collect::<String>();
        assert_eq!(
            (code, stderr.as_str()),
            (1, kotlinc.as_str()),
            "kotlinc {extra:?}"
        );

        let output = self.work.join("krusty");
        let result = self.krusty_with_jdk(jdk, "krusty", extra);
        let krusty = expected
            .iter()
            .map(|error| format!("error: {error}\n"))
            .collect::<String>();
        assert_eq!(result.status.code(), Some(1), "krusty {extra:?}");
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            "",
            "krusty {extra:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stderr),
            krusty,
            "krusty {extra:?}"
        );
        assert!(!output.exists(), "krusty wrote {}", output.display());
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
    assert_eq!(result.status.code(), Some(code), "kotlinc's exit status");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        format!("krusty: error: {message}\n")
    );
    assert!(!output.exists(), "krusty wrote {}", output.display());
}

#[test]
fn an_older_release_compiles_against_its_ct_sym_api_as_kotlinc_does() {
    let fixture = Fixture::with_source(FLIP);
    // The reference JDK is 21 or newer, so both are older releases its `ct.sym` describes. kotlinc
    // itself cannot read every release there: JDK 21's module descriptors for release 20 fail it.
    for (release, flip) in [
        (8, "flip()Ljava/nio/Buffer;"),
        (17, "flip()Ljava/nio/ByteBuffer;"),
    ] {
        let argument = format!("-Xjdk-release={release}");
        let kotlinc = format!("kotlinc-{release}");
        let krusty = format!("krusty-{release}");
        let (code, stderr) =
            common::kotlinc_compile(&fixture.arguments(&kotlinc, &[&argument])).expect("kotlinc");
        assert_eq!((code, stderr.as_str()), (0, ""), "kotlinc {argument}");
        let expected = classes(&fixture.work.join(&kotlinc));
        let (_, file_class) = expected
            .iter()
            .find(|(name, _)| name.ends_with("BoxKt.class"))
            .expect("kotlinc's file class");
        let descriptor = flip.replace("flip", "");
        assert!(
            file_class
                .windows(descriptor.len())
                .any(|window| window == descriptor.as_bytes()),
            "kotlinc {argument} calls {flip}"
        );

        let result = fixture.krusty(&krusty, &[&argument]);
        assert_eq!(result.status.code(), Some(0), "krusty {argument}");
        assert_eq!(
            String::from_utf8_lossy(&result.stderr),
            "",
            "krusty {argument}"
        );
        assert_eq!(classes(&fixture.work.join(&krusty)), expected, "{argument}");
    }
}

#[test]
fn a_release_the_jdk_does_not_describe_is_reported_in_kotlincs_words() {
    let fixture = Fixture::new();
    let jdk = fixture.jdk.clone();
    let not_supported = |release: &str| {
        format!("'-Xjdk-release={release}' option is not supported by used JDK: {jdk}")
    };

    // Before JDK 9 there is no module graph, so only the release is reported.
    fixture.assert_rejected_as_kotlinc_does(
        &jdk,
        &["-Xjdk-release=7", "-jvm-target", "1.8"],
        &[not_supported("7")],
    );
    let newer = (fixture.release + 1).to_string();
    fixture.assert_rejected_as_kotlinc_does(
        &jdk,
        &[&format!("-Xjdk-release={newer}")],
        &[
            "module java.base cannot be found in the module graph".to_string(),
            not_supported(&newer),
        ],
    );
}

/// The reference JDK, every file but `lib/<omitted>` linked into a scratch home.
fn jdk_without(fixture: &Fixture, omitted: &str) -> PathBuf {
    let home = fixture.work.join(format!("jdk-without-{omitted}"));
    let jdk = Path::new(&fixture.jdk);
    for directory in [jdk.to_path_buf(), jdk.join("lib")] {
        let target = home.join(directory.strip_prefix(jdk).expect("below the JDK home"));
        std::fs::create_dir_all(&target).expect("create the scratch home");
        for entry in std::fs::read_dir(&directory).expect("read the JDK home") {
            let source = entry.expect("JDK home entry").path();
            let name = source.file_name().expect("entry name");
            if (directory == jdk && name == "lib") || (directory != jdk && name == omitted) {
                continue;
            }
            link(&source, &target.join(name));
        }
    }
    home
}

#[cfg(unix)]
fn link(source: &Path, link: &Path) {
    std::os::unix::fs::symlink(source, link).expect("link a JDK file");
}

#[cfg(windows)]
fn link(source: &Path, link: &Path) {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, link)
    } else {
        std::os::windows::fs::symlink_file(source, link)
    }
    .expect("link a JDK file");
}

#[test]
fn a_jdk_without_ct_sym_is_reported_in_kotlincs_words() {
    let fixture = Fixture::new();
    let home = jdk_without(&fixture, "ct.sym");
    let home = home.to_string_lossy().into_owned();
    let missing = |release: u16| {
        format!(
            "this JDK does not have the 'ct.sym' file required for the '-Xjdk-release={release}' \
             option: {home}"
        )
    };
    fixture.assert_rejected_as_kotlinc_does(&home, &["-Xjdk-release=8"], &[missing(8)]);
    // kotlinc tells the JDK's own release from `ct.sym` too, so even that release needs the file.
    fixture.assert_rejected_as_kotlinc_does(
        &home,
        &[&format!("-Xjdk-release={}", fixture.release)],
        &[
            "module java.base cannot be found in the module graph".to_string(),
            missing(fixture.release),
        ],
    );
}

/// The JDK's own release reads its module image. kotlinc fails with an internal error on a home
/// without one; krusty reports the incomplete home and writes nothing.
#[test]
fn the_own_release_of_a_jdk_without_its_module_image_is_reported() {
    let fixture = Fixture::new();
    let home = jdk_without(&fixture, "modules");
    let release = format!("-Xjdk-release={}", fixture.release);
    let result = fixture.krusty_with_jdk(&home.to_string_lossy(), "krusty", &[&release]);
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&result.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        format!(
            "krusty: error: {release} is the JDK's own release, but {} has neither lib/modules \
             nor jre/lib/rt.jar\n",
            home.display()
        )
    );
    assert!(!fixture.work.join("krusty").exists());

    // An older release reads `ct.sym` alone, so the same home still compiles it.
    let older = fixture.krusty_with_jdk(&home.to_string_lossy(), "krusty-8", &["-Xjdk-release=8"]);
    assert_eq!(older.status.code(), Some(0), "{older:?}");
    assert_eq!(String::from_utf8_lossy(&older.stderr), "");
}

/// A release with no JDK selected (no `-jdk-home`, no `JAVA_HOME`) has no API to read. kotlinc
/// always runs on a JDK, so this is krusty's configuration error; nothing is written.
#[test]
fn a_release_without_a_jdk_is_reported() {
    let fixture = Fixture::new();
    let output = fixture.work.join("krusty");
    let result = std::process::Command::new(common::krusty_binary())
        .arg(fixture.work.join("Box.kt"))
        .arg("-d")
        .arg(&output)
        .arg("-Xjdk-release=8")
        .env_remove("JAVA_HOME")
        .output()
        .expect("run krusty");
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&result.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "krusty: error: -Xjdk-release=8 needs a JDK: pass -jdk-home or set JAVA_HOME\n"
    );
    assert!(!output.exists(), "krusty wrote {}", output.display());

    // The same command line with the JDK selected compiles.
    let selected = fixture.krusty("krusty", &["-Xjdk-release=8"]);
    assert_eq!(selected.status.code(), Some(0), "{selected:?}");
}
