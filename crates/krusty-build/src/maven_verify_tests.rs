//! `mvn test-compile krusty:verify` with the Maven plugin built from `tools/krusty-maven`: kotlinc
//! compiles and ships both kotlin-maven-plugin compilations, krusty compiles each beside it, and the
//! report counts what krusty compiled, what it does not support and how its classes compare.

use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
#[ignore = "downloads Maven plugins and the Kotlin compiler"]
fn maven_verify_goal_reports_krusty_against_kotlinc() {
    let mvn = std::env::var_os("KRUSTY_MAVEN_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("mvn"));
    let krusty = std::env::var_os("KRUSTY_MAVEN_TEST_BIN")
        .map(PathBuf::from)
        .expect("KRUSTY_MAVEN_TEST_BIN must name the built krusty CLI");
    let kotlin = std::env::var("KRUSTY_MAVEN_KOTLIN_VERSION").unwrap_or_else(|_| "2.4.20".into());
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/krusty-maven/pom.xml");
    let root = std::env::temp_dir().join(format!("krusty-maven-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write_verify_build(&root, &kotlin);

    // A version of its own, so the installed plugin never stands in for a released one.
    maven(
        &mvn,
        &root,
        &[
            "-f",
            &plugin.display().to_string(),
            "-Drevision=0.0.0-verify-test",
            "install",
        ],
    );
    let output = maven(
        &mvn,
        &root,
        &[
            "test-compile",
            "dev.krustythecompiler:krusty-maven-plugin:0.0.0-verify-test:verify",
            &format!("-Dkrusty.binary={}", krusty.display()),
        ],
    );

    let reports = output
        .lines()
        .filter_map(|line| line.strip_prefix("[INFO] "))
        .filter(|line| {
            line.starts_with("krusty verify: ")
                || line.starts_with("report: ")
                || line.starts_with("submit it")
        })
        .map(without_measurements)
        .collect::<Vec<_>>();
    let report_dir = root.join("target/krusty-verify/report");
    assert_eq!(
        reports,
        [
            "krusty verify: sample:compile: 2 of 2 classes byte-identical, output identical"
                .to_string(),
            "krusty verify: sample:test-compile: unsupported, left to kotlinc: krusty does not implement the kotlinc argument '-Wextra'".to_string(),
            "krusty verify: 1/2 compilations compiled (1 byte-identical), 0 failed, 1 unsupported; 100.0% of classes byte-identical".to_string(),
            format!("report: {}", report_dir.join("krusty-verify.html").display()),
            format!(
                "submit it to help prioritize krusty: open the report's issue button, or paste {} into https://github.com/qnox/krusty/issues/new",
                report_dir.join("krusty-verify-issue.md").display()
            ),
        ],
        "{output}"
    );

    // kotlinc compiled and shipped both compilations; krusty only compiled beside it.
    assert!(root.join("target/classes/app/Point.class").is_file());
    assert!(root
        .join("target/test-classes/app/PointCheck.class")
        .is_file());
    assert!(root
        .join("target/krusty-verify/main/classes/app/Point.class")
        .is_file());
    for report in ["krusty-verify.html", "krusty-verify.md"] {
        assert!(report_dir.join(report).is_file(), "{report}");
    }
    let issue = std::fs::read_to_string(report_dir.join("krusty-verify-issue.md")).expect("issue");
    assert!(!issue.contains("sample:"), "{issue}");
    assert!(!issue.contains("Point"), "{issue}");

    let _ = std::fs::remove_dir_all(&root);
}

/// Run Maven in batch mode, with `KRUSTY_MAVEN_SETTINGS` as its settings when set (a mirror for a
/// rate-limited Maven Central), and return what it printed.
fn maven(mvn: &Path, root: &Path, arguments: &[&str]) -> String {
    let mut command = Command::new(mvn);
    command.current_dir(root).arg("-B");
    if let Some(settings) = std::env::var_os("KRUSTY_MAVEN_SETTINGS") {
        command.arg("-s").arg(settings);
    }
    let output = command
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("cannot run {}: {error}", mvn.display()));
    let printed = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "mvn {arguments:?} failed:\n{printed}"
    );
    printed
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

/// One Kotlin project: the main compilation uses all-open through kotlin-maven-plugin's plugin
/// dependency, and the test compilation's own execution adds `-Wextra`, which krusty does not
/// support.
fn write_verify_build(root: &Path, kotlin: &str) {
    let write = |path: &str, text: &str| {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("directories");
        std::fs::write(file, text).expect("fixture file");
    };
    write(
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
    <plugins>
      <plugin>
        <groupId>org.jetbrains.kotlin</groupId>
        <artifactId>kotlin-maven-plugin</artifactId>
        <version>{kotlin}</version>
        <executions>
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
          </execution>
        </executions>
        <configuration>
          <jvmTarget>17</jvmTarget>
          <compilerPlugins><plugin>all-open</plugin></compilerPlugins>
          <pluginOptions><option>all-open:annotation=app.Open</option></pluginOptions>
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
    write(
        "src/main/kotlin/app/Point.kt",
        "package app\n\nannotation class Open\n\n@Open\nclass Point(val x: Int, val y: Int) {\n    fun sum(): Int = x + y\n}\n",
    );
    write(
        "src/test/kotlin/app/PointCheck.kt",
        "package app\n\nclass PointCheck {\n    fun check(): Boolean = Point(1, 2).sum() == 3\n}\n",
    );
}
