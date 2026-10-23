//! `mvn test-compile krusty:verify` with the Maven plugin from `tools/krusty-maven`: kotlinc
//! compiles and ships every kotlin-maven-plugin compilation, krusty compiles each beside it, and the
//! report counts what krusty compiled, what it does not support and how its classes compare. A
//! second build reports only itself, an output that would overlap kotlinc's is refused before
//! anything is written, and executions that share an output directory are reported unsupported.
//!
//! The plugin comes from the packaged release repository `KRUSTY_MAVEN_PLUGIN_REPOSITORY` (at
//! `KRUSTY_MAVEN_PLUGIN_VERSION`) when set, and is otherwise installed from the checkout.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The Kotlin release the sample builds use.
fn kotlin() -> String {
    std::env::var("KRUSTY_MAVEN_KOTLIN_VERSION").unwrap_or_else(|_| "2.4.20".into())
}

fn mvn() -> PathBuf {
    std::env::var_os("KRUSTY_MAVEN_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("mvn"))
}

/// The plugin's version, and the repository it resolves from (`None`: the local repository the
/// checkout's plugin was installed into, once per test process).
fn plugin() -> &'static (String, Option<PathBuf>) {
    static PLUGIN: OnceLock<(String, Option<PathBuf>)> = OnceLock::new();
    PLUGIN.get_or_init(|| {
        if let Some(repository) = std::env::var_os("KRUSTY_MAVEN_PLUGIN_REPOSITORY") {
            let version = std::env::var("KRUSTY_MAVEN_PLUGIN_VERSION")
                .expect("KRUSTY_MAVEN_PLUGIN_VERSION names the packaged plugin's version");
            return (version, Some(PathBuf::from(repository)));
        }
        // A version of its own, so the installed plugin never stands in for a released one.
        let version = "0.0.0-verify-test".to_string();
        let pom = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/krusty-maven/pom.xml");
        let (success, output) = maven(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            &[
                "-f",
                &pom.display().to_string(),
                &format!("-Drevision={version}"),
                "install",
            ],
        );
        assert!(success, "installing the plugin failed:\n{output}");
        (version, None)
    })
}

struct Sample {
    root: PathBuf,
}

impl Sample {
    /// A one-module Kotlin build: `build` goes into the POM's `<build>`, `executions` are
    /// kotlin-maven-plugin's and `configuration` its plugin-level configuration.
    fn new(name: &str, build: &str, executions: &str, configuration: &str) -> Sample {
        let root = std::env::temp_dir().join(format!("krusty-maven-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (_, repository) = plugin();
        let plugin_repositories = repository
            .as_ref()
            .map(|repository| {
                format!(
                    "<pluginRepositories><pluginRepository><id>krusty</id><url>file://{}</url></pluginRepository></pluginRepositories>",
                    repository.display()
                )
            })
            .unwrap_or_default();
        let kotlin = kotlin();
        let sample = Sample { root };
        sample.write(
            "pom.xml",
            &format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>sample</groupId>
  <artifactId>sample</artifactId>
  <version>1.0</version>
  <properties>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  {plugin_repositories}
  <dependencies>
    <dependency>
      <groupId>org.jetbrains.kotlin</groupId>
      <artifactId>kotlin-stdlib</artifactId>
      <version>{kotlin}</version>
    </dependency>
  </dependencies>
  <build>
    <sourceDirectory>src/main/kotlin</sourceDirectory>
    <testSourceDirectory>src/test/kotlin</testSourceDirectory>
    {build}
    <plugins>
      <plugin>
        <groupId>org.jetbrains.kotlin</groupId>
        <artifactId>kotlin-maven-plugin</artifactId>
        <version>{kotlin}</version>
        <executions>{executions}</executions>
        <configuration>
          <jvmTarget>17</jvmTarget>
          {configuration}
        </configuration>
        <dependencies>
          <dependency>
            <groupId>org.jetbrains.kotlin</groupId>
            <artifactId>kotlin-maven-allopen</artifactId>
            <version>{kotlin}</version>
          </dependency>
        </dependencies>
      </plugin>
    </plugins>
  </build>
</project>
"#
            ),
        );
        sample.write(
            "src/main/kotlin/app/Point.kt",
            "package app\n\nannotation class Open\n\n@Open\nclass Point(val x: Int, val y: Int) {\n    fun sum(): Int = x + y\n}\n",
        );
        sample.write(
            "src/test/kotlin/app/PointCheck.kt",
            "package app\n\nclass PointCheck {\n    fun check(): Boolean = Point(1, 2).sum() == 3\n}\n",
        );
        sample
    }

    fn write(&self, path: &str, text: &str) {
        let file = self.root.join(path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("directories");
        std::fs::write(file, text).expect("sample file");
    }

    /// Run Maven on the sample; `goals` may name the verify goal as `VERIFY`.
    fn maven(&self, goals: &[&str]) -> (bool, String) {
        let krusty = std::env::var_os("KRUSTY_MAVEN_TEST_BIN")
            .map(PathBuf::from)
            .expect("KRUSTY_MAVEN_TEST_BIN must name the built krusty CLI");
        let goal = format!(
            "dev.krustythecompiler:krusty-maven-plugin:{}:verify",
            plugin().0
        );
        let mut arguments = goals
            .iter()
            .map(|argument| {
                if *argument == "VERIFY" {
                    goal.as_str()
                } else {
                    argument
                }
            })
            .map(str::to_string)
            .collect::<Vec<_>>();
        arguments.push(format!("-Dkrusty.binary={}", krusty.display()));
        maven(
            &self.root,
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }

    fn succeed(&self, goals: &[&str]) -> String {
        let (success, output) = self.maven(goals);
        assert!(success, "mvn {goals:?} failed:\n{output}");
        output
    }

    fn report_dir(&self) -> PathBuf {
        self.root.join("target/krusty-verify/report")
    }

    /// The report lines a successful verify ends with.
    fn report_tail(&self, summary: &str) -> Vec<String> {
        vec![
            summary.to_string(),
            format!(
                "report: {}",
                self.report_dir().join("krusty-verify.html").display()
            ),
            format!(
                "submit it to help prioritize krusty: open the report's issue button, or paste {} into https://github.com/qnox/krusty/issues/new",
                self.report_dir().join("krusty-verify-issue.md").display()
            ),
        ]
    }
}

impl Drop for Sample {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const EXECUTIONS: &str = r#"
          <execution>
            <id>compile</id>
            <goals><goal>compile</goal></goals>
          </execution>
          <execution>
            <id>test-compile</id>
            <goals><goal>test-compile</goal></goals>
            <configuration>
              <args><arg>-Wextra</arg></args>
            </configuration>
          </execution>"#;

const ALL_OPEN: &str = r#"
          <compilerPlugins><plugin>all-open</plugin></compilerPlugins>
          <pluginOptions><option>all-open:annotation=app.Open</option></pluginOptions>"#;

/// The main compilation uses all-open through kotlin-maven-plugin's plugin dependency, and the test
/// compilation's own execution adds `-Wextra`, which krusty does not support.
#[test]
#[ignore = "downloads Maven plugins and the Kotlin compiler"]
fn maven_verify_goal_reports_krusty_against_kotlinc() {
    let sample = Sample::new("verify", "", EXECUTIONS, ALL_OPEN);
    let root = &sample.root;
    let output = sample.succeed(&["test-compile", "VERIFY"]);
    let mut expected = vec![
        "krusty verify: sample:compile: 2 of 2 classes byte-identical, output identical".to_string(),
        "krusty verify: sample:test-compile: unsupported, left to kotlinc: krusty does not implement the kotlinc argument '-Wextra'".to_string(),
    ];
    expected.extend(sample.report_tail(
        "krusty verify: 1/2 compilations compiled (1 byte-identical), 0 failed, 1 unsupported; 100.0% of classes byte-identical",
    ));
    assert_eq!(report_lines(&output), expected, "{output}");

    // kotlinc compiled and shipped both compilations; krusty only compiled beside it.
    assert!(root.join("target/classes/app/Point.class").is_file());
    assert!(root
        .join("target/test-classes/app/PointCheck.class")
        .is_file());
    assert!(root
        .join("target/krusty-verify/main/classes/app/Point.class")
        .is_file());
    for report in ["krusty-verify.html", "krusty-verify.md"] {
        assert!(sample.report_dir().join(report).is_file(), "{report}");
    }
    let issue =
        std::fs::read_to_string(sample.report_dir().join("krusty-verify-issue.md")).expect("issue");
    assert!(!issue.contains("sample:"), "{issue}");
    assert!(!issue.contains("Point"), "{issue}");
    // Both compilations carry kotlinc's time, the goal's own timed kotlinc compile, whose output
    // equals the build's.
    let records = root.join("target/krusty-verify/records");
    for file in ["sample_compile.json", "sample_test-compile.json"] {
        let record = std::fs::read_to_string(records.join(file)).expect("record");
        assert!(
            record.lines().any(|line| line
                .trim()
                .strip_prefix("\"kotlinc_millis\": ")
                .is_some_and(|millis| millis.trim_end_matches(',').parse::<u64>().is_ok())),
            "{record}"
        );
    }

    // A second build without the sources reports only itself: each compilation afresh as having no
    // sources, and nothing from the first build or from a record no compilation of this build wrote.
    std::fs::remove_dir_all(root.join("src")).expect("remove the sources");
    std::fs::write(records.join("removed_compile.json"), "{}").expect("an unrelated record");
    let output = sample.succeed(&["test-compile", "VERIFY"]);
    let mut expected = vec![
        "krusty verify: sample:compile: no Kotlin sources".to_string(),
        "krusty verify: sample:test-compile: no Kotlin sources".to_string(),
    ];
    expected.extend(sample.report_tail(
        "krusty verify: 0/0 compilations compiled (0 byte-identical), 0 failed, 0 unsupported, 2 without Kotlin sources; n/a of classes byte-identical",
    ));
    assert_eq!(report_lines(&output), expected, "{output}");
    let kotlin = kotlin();
    let no_sources = |goal: &str, source_set: &str| {
        format!(
            "{{\n  \"compilation\": \"sample:{goal}\",\n  \"source_set\": \"{source_set}\",\n  \"kotlin_version\": \"{kotlin}\",\n  \"outcome\": {{\n    \"status\": \"no-sources\"\n  }}\n}}\n"
        )
    };
    for (file, goal, source_set) in [
        ("sample_compile.json", "compile", "main"),
        ("sample_test-compile.json", "test-compile", "test"),
    ] {
        assert_eq!(
            std::fs::read_to_string(records.join(file)).expect("record"),
            no_sources(goal, source_set)
        );
    }
    assert!(!root.join("target/krusty-verify/main/classes").exists());
    let issue =
        std::fs::read_to_string(sample.report_dir().join("krusty-verify-issue.md")).expect("issue");
    assert!(issue.contains("| Compilations | 0 |\n"), "{issue}");
    assert!(
        issue.contains("| Without Kotlin sources | 2 |\n"),
        "{issue}"
    );
    assert!(!issue.contains("peak memory"), "{issue}");
}

/// The goal times kotlinc by compiling each compilation once more; output other than the build's
/// means it did not compile what the build compiled, and the goal fails rather than report that
/// compile's time.
#[test]
#[ignore = "downloads Maven plugins and the Kotlin compiler"]
fn maven_verify_goal_fails_when_its_kotlinc_compile_differs_from_the_build() {
    let sample = Sample::new("kotlinc-differs", "", EXECUTIONS, ALL_OPEN);
    sample.succeed(&["test-compile"]);
    let class = sample.root.join("target/classes/app/Point.class");
    let mut bytes = std::fs::read(&class).expect("kotlinc's class");
    bytes.push(0);
    std::fs::write(&class, bytes).expect("a class kotlinc would not write");
    let (success, output) = sample.maven(&["VERIFY"]);
    assert!(!success, "{output}");
    let errors = output
        .lines()
        .filter(|line| line.starts_with("[ERROR] Failed to execute goal"))
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "{output}");
    assert!(
        errors[0].ends_with(": kotlinc as krusty:verify runs it wrote [app/Point.class] of sample:compile differently from the build; krusty:verify reads kotlin-maven-plugin's configuration incompletely -> [Help 1]"),
        "{output}"
    );
    assert!(!sample.root.join("target/krusty-verify/records").exists());
}

/// A build whose own output directory is where the goal would write, or reaches it through a
/// link, fails the goal before krusty clears or writes anything, and kotlinc's classes stay as
/// they were.
#[test]
#[ignore = "downloads Maven plugins and the Kotlin compiler"]
fn maven_verify_goal_refuses_an_output_that_overlaps_kotlincs() {
    let configured = Sample::new(
        "configured-output",
        "<outputDirectory>${project.build.directory}/krusty-verify/main/classes</outputDirectory>",
        EXECUTIONS,
        ALL_OPEN,
    );
    let reference = configured.root.join("target/krusty-verify/main/classes");
    expect_refusal(&configured, &reference, &reference);

    let linked = Sample::new("linked-output", "", EXECUTIONS, ALL_OPEN);
    linked.succeed(&["test-compile"]);
    let reference = linked.root.join("target/classes");
    std::fs::create_dir_all(linked.root.join("target/krusty-verify")).expect("verify directory");
    // target/krusty-verify/main/classes is then target/classes.
    std::os::unix::fs::symlink("..", linked.root.join("target/krusty-verify/main")).expect("link");
    expect_refusal(
        &linked,
        &reference,
        &linked.root.join("target/krusty-verify/main/classes"),
    );
}

fn expect_refusal(sample: &Sample, reference: &Path, destination: &Path) {
    sample.succeed(&["test-compile"]);
    let before = files(reference);
    assert!(
        before.contains_key(Path::new("app/Point.class")),
        "{before:?}"
    );
    let (success, output) = sample.maven(&["VERIFY"]);
    assert!(!success, "{output}");
    let refusal = format!(
        "[INFO] error: -d {} overlaps --reference {}; kotlinc's output, krusty's output and the record must be separate",
        destination.display(),
        reference.display()
    );
    let refusals = output
        .lines()
        .filter(|line| line.starts_with("[INFO] error: "))
        .collect::<Vec<_>>();
    assert_eq!(refusals, [refusal.as_str()], "{output}");
    assert!(
        output.contains("krusty verify-compile exited with 2"),
        "{output}"
    );
    assert_eq!(files(reference), before);
    assert!(!sample.root.join("target/krusty-verify/records").exists());
    assert!(!sample.report_dir().exists());
}

/// Two executions of `compile` write into one output directory, where kotlinc's output of each
/// cannot be told apart: each is reported unsupported, by its execution.
#[test]
#[ignore = "downloads Maven plugins and the Kotlin compiler"]
fn maven_verify_goal_reports_executions_sharing_an_output_as_unsupported() {
    let executions = r#"
          <execution>
            <id>compile</id>
            <goals><goal>compile</goal></goals>
          </execution>
          <execution>
            <id>extra</id>
            <goals><goal>compile</goal></goals>
            <configuration>
              <sourceDirs><sourceDir>src/extra/kotlin</sourceDir></sourceDirs>
            </configuration>
          </execution>
          <execution>
            <id>test-compile</id>
            <goals><goal>test-compile</goal></goals>
          </execution>"#;
    let sample = Sample::new("shared-output", "", executions, ALL_OPEN);
    sample.write(
        "src/extra/kotlin/app/Extra.kt",
        "package app\n\nclass Extra(val point: Point)\n",
    );
    let output = sample.succeed(&["test-compile", "VERIFY"]);
    let reason = "krusty: kotlin-maven-plugin runs 2 executions of 'compile' into one output directory, so kotlinc's output of each cannot be told apart";
    let mut expected = vec![
        format!("krusty verify: sample:compile@compile: unsupported, left to kotlinc: {reason}"),
        format!("krusty verify: sample:compile@extra: unsupported, left to kotlinc: {reason}"),
        "krusty verify: sample:test-compile: 1 of 1 classes byte-identical, output identical"
            .to_string(),
    ];
    expected.extend(sample.report_tail(
        "krusty verify: 1/3 compilations compiled (1 byte-identical), 0 failed, 2 unsupported; 100.0% of classes byte-identical",
    ));
    assert_eq!(report_lines(&output), expected, "{output}");
    assert!(sample.root.join("target/classes/app/Extra.class").is_file());
}

/// Run Maven in batch mode, with `KRUSTY_MAVEN_SETTINGS` as its settings when set (a mirror for a
/// rate-limited Maven Central): whether it succeeded, and what it printed.
fn maven(directory: &Path, arguments: &[&str]) -> (bool, String) {
    let mut command = Command::new(mvn());
    command.current_dir(directory).arg("-B");
    if let Some(settings) = std::env::var_os("KRUSTY_MAVEN_SETTINGS") {
        command.arg("-s").arg(settings);
    }
    let output = command
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("cannot run {}: {error}", mvn().display()));
    let printed = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), printed)
}

/// Every file under `directory`, with its bytes.
fn files(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(directory: &Path, base: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(directory).expect("read a directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, base, files);
            } else {
                let bytes = std::fs::read(&path).expect("read a file");
                files.insert(
                    path.strip_prefix(base).expect("under base").to_path_buf(),
                    bytes,
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(directory, directory, &mut files);
    files
}

/// The goal's report lines, without the time and memory they measured.
fn report_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("[INFO] "))
        .filter(|line| {
            line.starts_with("krusty verify: ")
                || line.starts_with("report: ")
                || line.starts_with("submit it")
        })
        .map(without_measurements)
        .collect()
}

/// A report line without the time and memory it measured: `… (0.4 s, 94 MB peak)`.
fn without_measurements(line: &str) -> String {
    match line.rfind(" (") {
        Some(start) if line.ends_with(" peak)") || line.ends_with(" s)") => {
            line[..start].to_string()
        }
        _ => line.to_string(),
    }
}
