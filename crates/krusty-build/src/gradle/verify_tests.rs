//! `gradle krustyVerify` against the released plugin: kotlinc compiles and ships every compilation,
//! krusty compiles each beside it, and the report counts what krusty compiled, what it does not
//! support and how its classes compare.

use super::tests::{invocations, IntegrationFixture};
use std::path::Path;

#[test]
#[ignore = "downloads Gradle and the Kotlin Gradle plugin"]
fn verify_task_reports_krusty_against_kotlinc() {
    let fixture = IntegrationFixture::new("krusty-verify", write_verify_build);
    let root = &fixture.root;
    let output = fixture
        .build()
        .tasks(["krustyVerify"])
        .run_output()
        .unwrap_or_else(|error| panic!("{error}"));

    let reports = output
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("krusty verify: ")
                || line.starts_with("report: ")
                || line.starts_with("submit it")
        })
        .map(without_measurements)
        .collect::<Vec<_>>();
    let report_dir = root.join("build/reports/krusty-verify");
    assert_eq!(
        reports,
        [
            "krusty verify: :compileKotlin: 1 of 1 classes byte-identical, output identical".to_string(),
            "krusty verify: :compileTestKotlin: unsupported, left to kotlinc: krusty does not support compilerOptions.extraWarnings".to_string(),
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
    assert!(root
        .join("build/classes/kotlin/main/app/Point.class")
        .is_file());
    assert!(root
        .join("build/classes/kotlin/test/app/PointTest.class")
        .is_file());
    assert!(root
        .join("build/krusty/verify/compileKotlin/classes/app/Point.class")
        .is_file());
    let runs = invocations(&std::fs::read_to_string(&fixture.log).expect("invocation log"));
    let commands = runs
        .iter()
        .map(|run| run.first().map(String::as_str).unwrap_or(""))
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        ["verify-compile", "verify-compile", "verify-report"],
        "{runs:?}"
    );

    for report in ["krusty-verify.html", "krusty-verify.md"] {
        assert!(report_dir.join(report).is_file(), "{report}");
    }
    let issue = std::fs::read_to_string(report_dir.join("krusty-verify-issue.md")).expect("issue");
    assert!(!issue.contains("compileKotlin"), "{issue}");
    assert!(!issue.contains("Point"), "{issue}");

    let _ = std::fs::remove_dir_all(root);
}

/// Each invocation describes the build as it is then: an unchanged one compiles in full again,
/// and once the sources are gone, every
/// compilation is recorded afresh as having none, and neither an earlier record nor its
/// measurements survive in the records or the report.
#[test]
#[ignore = "downloads Gradle and the Kotlin Gradle plugin"]
fn verify_task_reports_only_the_current_invocation() {
    let fixture = IntegrationFixture::new("krusty-verify-fresh", write_verify_build);
    let root = &fixture.root;
    fixture
        .build()
        .tasks(["krustyVerify"])
        .run_output()
        .unwrap_or_else(|error| panic!("{error}"));
    let records = root.join("build/krusty/verify");
    assert!(records
        .join("compileKotlin/classes/app/Point.class")
        .is_file());

    // An unchanged second invocation still has kotlinc compile everything: Gradle hands the Kotlin
    // task no incremental changes, so its time is a full compile's again.
    let warm = fixture
        .build()
        .tasks(["krustyVerify", "--info"])
        .run_output()
        .unwrap_or_else(|error| panic!("{error}"));
    for task in [":compileKotlin", ":compileTestKotlin"] {
        assert!(
            warm.contains(&format!(
                "The input changes require a full rebuild for incremental task '{task}'."
            )),
            "{warm}"
        );
    }

    std::fs::remove_dir_all(root.join("src")).expect("remove the sources");
    let output = fixture
        .build()
        .tasks(["krustyVerify"])
        .run_output()
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        output.contains("Reusing configuration cache."),
        "the second invocation reuses the configuration cache:\n{output}"
    );
    let reports = output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("krusty verify: "))
        .collect::<Vec<_>>();
    assert_eq!(
        reports,
        [
            "krusty verify: :compileKotlin: no Kotlin sources",
            "krusty verify: :compileTestKotlin: no Kotlin sources",
            "krusty verify: 0/0 compilations compiled (0 byte-identical), 0 failed, 0 unsupported, 2 without Kotlin sources; n/a of classes byte-identical",
        ],
        "{output}"
    );

    let kgp = std::env::var("KRUSTY_GRADLE_KGP_VERSION").unwrap_or_else(|_| "2.4.20".into());
    let mut inventory = std::fs::read_dir(&records)
        .expect("records")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| {
            (
                path.file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read_to_string(&path).expect("record"),
            )
        })
        .collect::<Vec<_>>();
    inventory.sort();
    let no_sources = |task: &str, source_set: &str| {
        format!(
            "{{\n  \"compilation\": \":{task}\",\n  \"source_set\": \"{source_set}\",\n  \"kotlin_version\": \"{kgp}\",\n  \"outcome\": {{\n    \"status\": \"no-sources\"\n  }}\n}}\n"
        )
    };
    assert_eq!(
        inventory,
        [
            (
                "compileKotlin.json".to_string(),
                no_sources("compileKotlin", "main")
            ),
            (
                "compileTestKotlin.json".to_string(),
                no_sources("compileTestKotlin", "test")
            ),
        ]
    );
    // verify-compile cleared the earlier invocation's classes and compiled nothing.
    assert!(!records.join("compileKotlin/classes").exists());

    let report_dir = root.join("build/reports/krusty-verify");
    let mut written = std::fs::read_dir(&report_dir)
        .expect("report")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    written.sort();
    assert_eq!(
        written,
        [
            "krusty-verify-issue.md",
            "krusty-verify.html",
            "krusty-verify.md"
        ]
    );
    let issue = std::fs::read_to_string(report_dir.join("krusty-verify-issue.md")).expect("issue");
    assert!(issue.contains("| Compilations | 0 |\n"), "{issue}");
    assert!(
        issue.contains("| Without Kotlin sources | 2 |\n"),
        "{issue}"
    );
    assert!(!issue.contains("kotlinc / krusty time"), "{issue}");
    assert!(!issue.contains("peak memory"), "{issue}");

    let _ = std::fs::remove_dir_all(root);
}

/// A Kotlin Gradle plugin release krusty does not support (exactly, not by its nearest release),
/// from the packaged plugin: kotlinc compiles both compilations and ships them, and each gets a
/// fresh unsupported record although its request is never translated for krusty.
#[test]
#[ignore = "downloads Gradle and the Kotlin Gradle plugin"]
fn verify_task_reports_an_unsupported_kotlin_gradle_plugin() {
    let fixture = IntegrationFixture::new("krusty-verify-kgp", write_unsupported_kgp_build);
    let root = &fixture.root;
    let output = fixture
        .build()
        .tasks(["krustyVerify"])
        .run_output()
        .unwrap_or_else(|error| panic!("{error}"));
    let supported = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kotlin-versions"),
    )
    .expect("the kotlin-versions manifest")
    .lines()
    .filter_map(|line| line.split_whitespace().next())
    .filter(|version| !version.starts_with('#'))
    .map(str::to_string)
    .collect::<Vec<_>>();
    let (last, earlier) = supported.split_last().expect("a supported release");
    let reason = format!(
        "unsupported Kotlin Gradle plugin {UNSUPPORTED_KGP}; expected {}, or {last}",
        earlier.join(", ")
    );
    let reports = output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("krusty verify: "))
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        reports,
        [
            format!("krusty verify: :compileKotlin: unsupported, left to kotlinc: {reason}"),
            format!("krusty verify: :compileTestKotlin: unsupported, left to kotlinc: {reason}"),
            "krusty verify: 0/2 compilations compiled (0 byte-identical), 0 failed, 2 unsupported; n/a of classes byte-identical".to_string(),
        ],
        "{output}"
    );
    assert!(root
        .join("build/classes/kotlin/main/app/Point.class")
        .is_file());
    assert!(root
        .join("build/classes/kotlin/test/app/PointTest.class")
        .is_file());
    let issue =
        std::fs::read_to_string(root.join("build/reports/krusty-verify/krusty-verify-issue.md"))
            .expect("issue");
    assert!(issue.contains(&format!("| {reason} | 2 |\n")), "{issue}");

    let _ = std::fs::remove_dir_all(root);
}

/// Projects that each request the plugin load it in a classloader of their own, so each holds its
/// own copy of the plugin's classes; one report still covers every project's compilations.
#[test]
#[ignore = "downloads Gradle and the Kotlin Gradle plugin"]
fn verify_task_reports_projects_that_each_load_the_plugin() {
    let fixture = IntegrationFixture::new("krusty-verify-projects", write_projects_build);
    let output = fixture
        .build()
        .tasks(["krustyVerify"])
        .run_output()
        .unwrap_or_else(|error| panic!("{error}"));
    let mut reports = output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("krusty verify: "))
        .map(without_measurements)
        .collect::<Vec<_>>();
    // The report comes last; the compilations' own lines arrive in whatever order their verify
    // tasks ran.
    let summary = reports.pop();
    reports.sort();
    assert_eq!(
        (reports, summary),
        (
            [
                ":first:compileKotlin: 1 of 1 classes byte-identical, output identical",
                ":first:compileTestKotlin: no Kotlin sources",
                ":second:compileKotlin: 1 of 1 classes byte-identical, output identical",
                ":second:compileTestKotlin: no Kotlin sources",
            ]
            .iter()
            .map(|line| format!("krusty verify: {line}"))
            .collect::<Vec<_>>(),
            Some("krusty verify: 2/2 compilations compiled (2 byte-identical), 0 failed, 0 unsupported, 2 without Kotlin sources; 100.0% of classes byte-identical".to_string()),
        ),
        "{output}"
    );
    let _ = std::fs::remove_dir_all(&fixture.root);
}

fn write_projects_build(root: &Path, plugin_version: &str, kgp_version: &str) {
    write_verify_build(root, plugin_version, kgp_version);
    let write = |rel: &str, body: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let body = body
            .replace("PLUGIN_VERSION", plugin_version)
            .replace("KGP_VERSION", kgp_version);
        std::fs::write(path, body).expect("write");
    };
    std::fs::remove_dir_all(root.join("src")).expect("remove the single project's sources");
    let settings = root.join("settings.gradle.kts");
    let mut text = std::fs::read_to_string(&settings).expect("settings");
    text.push_str("include(\":first\", \":second\")\n");
    std::fs::write(&settings, text).expect("settings");
    write(
        "build.gradle.kts",
        r#"
plugins {
    kotlin("jvm") version "KGP_VERSION" apply false
}
"#,
    );
    for project in ["first", "second"] {
        write(
            &format!("{project}/build.gradle.kts"),
            r#"
plugins {
    kotlin("jvm")
    id("dev.krustythecompiler") version "PLUGIN_VERSION"
}

repositories {
    mavenCentral()
}
"#,
        );
        write(
            &format!("{project}/src/main/kotlin/{project}/Point.kt"),
            &format!("package {project}\n\nclass Point(val x: Int, val y: Int)\n"),
        );
    }
}

/// A Kotlin Gradle plugin release that exists and that the kotlin-versions manifest does not list.
const UNSUPPORTED_KGP: &str = "2.3.20";

fn write_unsupported_kgp_build(root: &Path, plugin_version: &str, _kgp_version: &str) {
    write_verify_build(root, plugin_version, UNSUPPORTED_KGP);
    // The extra-warnings switch is beside the point here; the plugin release alone is refused.
    let build = root.join("build.gradle.kts");
    let script = std::fs::read_to_string(&build).expect("build script");
    let plain = script
        .split("// krusty leaves a compile asking for extra warnings")
        .next()
        .expect("the build before the warning switch")
        .to_string();
    std::fs::write(&build, plain).expect("build script");
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

fn write_verify_build(root: &Path, plugin_version: &str, kgp_version: &str) {
    let write = |rel: &str, body: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let body = body
            .replace("PLUGIN_VERSION", plugin_version)
            .replace("KGP_VERSION", kgp_version);
        std::fs::write(path, body).expect("write");
    };
    write(
        "settings.gradle.kts",
        r#"
pluginManagement {
    repositories {
        providers.gradleProperty("krusty.plugin.repository").orNull?.let {
            maven { url = uri(it) }
        }
        gradlePluginPortal()
        mavenCentral()
    }
}

rootProject.name = "verify-consumer"
"#,
    );
    write(
        "build.gradle.kts",
        r#"
plugins {
    kotlin("jvm") version "KGP_VERSION"
    id("dev.krustythecompiler") version "PLUGIN_VERSION"
}

repositories {
    mavenCentral()
}

// krusty leaves a compile asking for extra warnings to kotlinc: verify reports it as unsupported.
tasks.named<org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile>("compileTestKotlin") {
    compilerOptions.extraWarnings.set(true)
}
"#,
    );
    write(
        "src/main/kotlin/app/Point.kt",
        "package app\n\nclass Point(val x: Int, val y: Int)\n",
    );
    write(
        "src/test/kotlin/app/PointTest.kt",
        "package app\n\nclass PointTest {\n    fun origin() = Point(0, 0)\n}\n",
    );
}
