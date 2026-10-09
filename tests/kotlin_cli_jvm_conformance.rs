//! Kotlin's own command-line test corpus, `compiler/testData/cli/jvm`, through the krusty binary.
//!
//! Each case is an `.args` file (one argument per line, `---` separating consecutive invocations)
//! and the `.out` file kotlinc's `AbstractCliTest` compares against: everything the compiler wrote
//! to stderr, normalized, followed by the exit code's name. That covers the whole command-line
//! surface a build tool sees: argument errors and warnings, help text, configuration errors,
//! source discovery, and the diagnostics a compilation reports. The runner mirrors
//! `AbstractCliTest.readArgs` and `CompilerTestUtil.normalizeCompilerOutput` so the `.out` files are
//! the oracle as committed upstream at the reference tag.
//!
//! Outcomes ratchet like the box corpus. `tests/cli_expected_failures/jvm/<version>.txt` lists the
//! cases krusty does not reproduce yet; a case leaving or joining that list fails until the list is
//! updated (`KRUSTY_BLESS_CLI_EXPECTATIONS=1` rewrites it from a full run).
//! `tests/cli_expected_not_applicable/jvm/<version>.txt` lists the cases kotlinc itself does not
//! reproduce outside JetBrains' test environment, each under a comment saying why;
//! `the_reference_compiler_reproduces_every_applicable_cli_case` holds that list honest by running
//! the reference kotlinc through the same runner.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;

use krusty::kotlin_version::KotlinVersion;

use super::box_ratchet;
use super::common;

/// kotlinc's `ExitCode` names, which every `.out` file ends with.
fn exit_code_name(code: Option<i32>) -> String {
    match code {
        Some(0) => "OK".to_string(),
        Some(1) => "COMPILATION_ERROR".to_string(),
        Some(2) => "INTERNAL_ERROR".to_string(),
        Some(3) => "SCRIPT_EXECUTION_ERROR".to_string(),
        Some(137) => "OOM_ERROR".to_string(),
        Some(code) => format!("exit code {code}"),
        None => "killed by a signal".to_string(),
    }
}

/// The JetBrains checkout the box corpus was provisioned from; `just box-corpus` places the CLI
/// corpus and the third-party annotation sources in the same checkout.
fn checkout_root() -> PathBuf {
    let box_dir = krusty::toolchain::box_corpus_dir()
        .expect("the Kotlin box corpus is provisioned (`just box-corpus <version>`)");
    let root = box_dir
        .ancestors()
        .nth(4)
        .filter(|_| box_dir.ends_with("compiler/testData/codegen/box"))
        .unwrap_or_else(|| panic!("{} is not a JetBrains checkout", box_dir.display()))
        .to_path_buf();
    assert!(
        root.join("compiler/testData/cli/jvm").is_dir(),
        "{} has no compiler/testData/cli/jvm; re-provision it with `just box-corpus <version>`",
        root.display()
    );
    root
}

fn cli_cases(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
            .map(|entry| entry.expect("directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "args")
            {
                out.push(path);
            }
        }
    }
    let mut cases = Vec::new();
    walk(&root.join("compiler/testData/cli/jvm"), &mut cases);
    cases
}

fn case_key(root: &Path, args_file: &Path) -> String {
    box_ratchet::corpus_key(&root.join("compiler/testData/cli/jvm"), args_file)
}

/// The compiler under test and the values the corpus placeholders stand for.
struct Toolchain {
    /// The executable each invocation runs.
    compiler: PathBuf,
    /// The checkout root: the working directory, so diagnostics render relative to it as in
    /// JetBrains' test runs.
    root: PathBuf,
    reference: KotlinVersion,
    /// `$JDK_1_8`, `$JDK_11_0`, `$JDK_17$`, `$JDK_21` by Java feature release.
    jdks: BTreeMap<u32, PathBuf>,
    /// `$ALLOPEN-COMPILER-PLUGIN-JAR$` and the other dist jars, with their placeholder.
    dist_jars: Vec<(&'static str, PathBuf)>,
    /// What the compiler's home directory renders as (`$PROJECT_DIR$`), when it has one.
    home: Option<PathBuf>,
    /// The running JVM's `java.runtime.version`, for the reference compiler's `-version` line.
    jvm_version: Option<String>,
}

const DIST_JARS: &[(&str, &str)] = &[
    (
        "$ALLOPEN-COMPILER-PLUGIN-JAR$",
        "allopen-compiler-plugin.jar",
    ),
    ("$NOARG-COMPILER-PLUGIN-JAR$", "noarg-compiler-plugin.jar"),
    ("$LOMBOK-COMPILER-PLUGIN-JAR$", "lombok-compiler-plugin.jar"),
    ("$KOTLIN-REFLECT-JAR$", "kotlin-reflect.jar"),
];

impl Toolchain {
    fn new(compiler: PathBuf, home: Option<PathBuf>, jvm_version: Option<String>) -> Self {
        let dist_jars = DIST_JARS
            .iter()
            .filter_map(|(placeholder, name)| {
                krusty::toolchain::dist_jar(name).map(|jar| (*placeholder, jar))
            })
            .collect();
        let root = checkout_root();
        link_dist(&root);
        Toolchain {
            compiler,
            root,
            reference: krusty::kotlin_version::target(),
            jdks: available_jdks(),
            dist_jars,
            home,
            jvm_version,
        }
    }
}

/// Older corpora name the dist jars by the path they have in a JetBrains checkout,
/// `dist/kotlinc/lib/<jar>`, relative to the working directory. Link that path to the reference
/// dist, whose jars are the ones the placeholders of newer corpora expand to.
fn link_dist(root: &Path) {
    let link = root.join("dist/kotlinc");
    if link.exists() {
        return;
    }
    let kotlinc = krusty::toolchain::kotlinc_path().expect("the reference kotlinc is provisioned");
    let home = kotlinc
        .parent()
        .and_then(Path::parent)
        .expect("kotlinc lives in <home>/bin");
    std::fs::create_dir_all(root.join("dist")).expect("create the checkout's dist directory");
    match std::os::unix::fs::symlink(home, &link) {
        Ok(()) => {}
        // Another test process linked it first.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("link {} to {}: {error}", link.display(), home.display()),
    }
}

/// JDK homes by feature release: `JAVA_HOME` for its own release, and `KRUSTY_JDK_<release>_HOME`
/// for any other the corpus names.
fn available_jdks() -> BTreeMap<u32, PathBuf> {
    let mut jdks = BTreeMap::new();
    if let Some(home) = std::env::var_os("JAVA_HOME").filter(|home| !home.is_empty()) {
        let home = PathBuf::from(home);
        if let Some(release) = jdk_release(&home) {
            jdks.insert(release, home);
        }
    }
    for release in [8, 11, 17, 21] {
        if let Some(home) = std::env::var_os(format!("KRUSTY_JDK_{release}_HOME")) {
            jdks.insert(release, PathBuf::from(home));
        }
    }
    jdks
}

/// The feature release a JDK's `release` file declares (`JAVA_VERSION="17.0.2"` → 17, `"1.8.0"` → 8).
fn jdk_release(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let version = text
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?
        .trim_matches('"');
    let mut parts = version.split(['.', '_', '+', '-']);
    match parts.next()?.parse().ok()? {
        1 => parts.next()?.parse().ok(),
        release => Some(release),
    }
}

/// What a case does when it runs.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail(String),
    /// The case needs something this environment cannot supply.
    NotApplicable(String),
}

/// One invocation's arguments, with every placeholder expanded the way `AbstractCliTest.readArg`
/// expands it.
struct Invocations {
    groups: Vec<Vec<String>>,
}

fn read_args(toolchain: &Toolchain, args_file: &Path, temp: &Path) -> Result<Invocations, String> {
    let test_data = args_file.parent().expect("an .args file has a directory");
    let text = std::fs::read_to_string(args_file)
        .unwrap_or_else(|error| panic!("read {}: {error}", args_file.display()));
    let mut groups = vec![Vec::new()];
    for line in text.lines().filter(|line| !line.is_empty()) {
        let jdk_line = match line {
            "$JDK_1_8" => Some(8),
            "$JDK_11_0" => Some(11),
            "$JDK_21" => Some(21),
            _ => None,
        };
        if let Some(release) = jdk_line {
            let home = toolchain
                .jdks
                .get(&release)
                .ok_or_else(|| format!("needs JDK {release} (set KRUSTY_JDK_{release}_HOME)"))?;
            groups.last_mut().unwrap().push(home.display().to_string());
            continue;
        }
        if line == "---" {
            groups.push(Vec::new());
            continue;
        }
        // `\:` is a literal colon; any other `:` separates paths.
        let separated = line
            .replace("\\:", "$COLON$")
            .replace(':', if cfg!(windows) { ";" } else { ":" })
            .replace("$COLON$", ":");
        let mut argument = replace_test_paths(toolchain, &separated, test_data, temp)?;
        for prefix in ["@", "-Xbuild-file="] {
            if let Some(path) = argument.strip_prefix(prefix) {
                let path = PathBuf::from(path);
                if path.is_file() {
                    let contents = std::fs::read_to_string(&path)
                        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                    let copy = temp.join(format!(
                        "{}.expanded{}",
                        path.file_name().unwrap().to_string_lossy(),
                        if prefix == "@" { "" } else { ".xml" }
                    ));
                    std::fs::write(
                        &copy,
                        replace_test_paths(toolchain, &contents, test_data, temp)?,
                    )
                    .unwrap_or_else(|error| panic!("write {}: {error}", copy.display()));
                    argument = format!("{prefix}{}", copy.display());
                }
                break;
            }
        }
        groups.last_mut().unwrap().push(argument);
    }
    Ok(Invocations { groups })
}

fn replace_test_paths(
    toolchain: &Toolchain,
    text: &str,
    test_data: &Path,
    temp: &Path,
) -> Result<String, String> {
    let third_party = |name: &str| toolchain.root.join("third-party").join(name);
    let mut replaced = text
        .replace("$TEMP_DIR$", &temp.display().to_string())
        .replace("$TESTDATA_DIR$", &test_data.display().to_string())
        .replace(
            "$FOREIGN_ANNOTATIONS_DIR$",
            &third_party("annotations").display().to_string(),
        )
        .replace(
            "$FOREIGN_JAVA8_ANNOTATIONS_DIR$",
            &third_party("java8-annotations").display().to_string(),
        )
        .replace(
            "$JSR_305_DECLARATIONS$",
            &third_party("jsr305").display().to_string(),
        );
    for (placeholder, jar) in &toolchain.dist_jars {
        replaced = replaced.replace(placeholder, &jar.display().to_string());
    }
    if replaced.contains("$JDK_17$") {
        let home = toolchain
            .jdks
            .get(&17)
            .ok_or("needs JDK 17 (set KRUSTY_JDK_17_HOME)")?;
        replaced = replaced.replace("$JDK_17$", &home.display().to_string());
    }
    if let Some(start) = replaced.find('$') {
        let rest = &replaced[start + 1..];
        if let Some(end) = rest.find('$') {
            let name = &rest[..end];
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                return Err(format!(
                    "needs ${name}$, which this environment does not supply"
                ));
            }
        }
    }
    Ok(replaced)
}

/// `CompilerTestUtil.normalizeCompilerOutput` plus `AbstractCliTest.getNormalizedCompilerOutput`.
fn normalize(toolchain: &Toolchain, output: &str, test_data: &Path, temp: &Path) -> String {
    let abi = format!(
        "{}.{}.0",
        toolchain.reference.major, toolchain.reference.minor
    );
    let abi_next = format!(
        "{}.{}.0",
        toolchain.reference.major,
        toolchain.reference.minor + 1
    );
    let mut text = output
        .replace("\r\n", "\n")
        .replace(&test_data.display().to_string(), "$TESTDATA_DIR$");
    for (placeholder, jar) in &toolchain.dist_jars {
        text = text.replace(&jar.display().to_string(), placeholder);
    }
    text = text
        .replace(&temp.display().to_string(), "$TMP_DIR$")
        .replace('\\', "/");
    for (release, home) in &toolchain.jdks {
        let name = match release {
            8 => "$JDK_1_8",
            11 => "$JDK_11",
            17 => "$JDK_17",
            21 => "$JDK_21",
            _ => continue,
        };
        text = text.replace(&home.display().to_string(), name);
    }
    // kotlinc replaces the directories after the versions; here the cached checkout and compiler
    // paths contain the version, so the directories go first.
    if let Some(home) = &toolchain.home {
        text = text.replace(&home.display().to_string(), "$PROJECT_DIR$");
        if let Some(parent) = home.parent() {
            text = text.replace(&parent.display().to_string(), "$DIST_DIR$");
        }
    }
    text = text.replace(&toolchain.root.display().to_string(), "$USER_DIR$");
    if let Some(version) = &toolchain.jvm_version {
        text = text.replace(version.as_str(), "$JVM_VERSION$");
    }
    // `$VERSION$` is not replaced here: JetBrains' test build has a snapshot version, so a release
    // number in the output (`deprecated since Kotlin 2.4.20`) is literal there. The expected side
    // spells the compiler version back instead ([`expected_output`]).
    text = replace_version(&text, &format!(" {abi}"), " $ABI_VERSION$");
    text = text.replace(&format!(" {abi_next}"), " $ABI_VERSION_NEXT$");
    const DURATION: &str = "info: executable production duration: ";
    text.lines()
        .filter(|line| {
            !line.starts_with("log4j:WARN")
                && !line.starts_with("Picked up JAVA_TOOL_OPTIONS:")
                && !line.starts_with("Picked up _JAVA_OPTIONS:")
        })
        .map(|line| match line.find(DURATION) {
            Some(at) => format!("{}{DURATION}[time]", &line[..at]),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// kotlinc's `" <abi>(?!-)"` replacement: the metadata version, unless a qualifier follows it.
fn replace_version(text: &str, version: &str, placeholder: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(version) {
        let after = &rest[at + version.len()..];
        out.push_str(&rest[..at]);
        if after.starts_with('-') {
            out.push_str(version);
        } else {
            out.push_str(placeholder);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// `assertEqualsToFile` ignores trailing whitespace on each line and around the whole text.
fn comparable(text: &str) -> String {
    text.replace("\r\n", "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn run_case(toolchain: &Toolchain, args_file: &Path) -> Outcome {
    if args_file.with_extension("env").exists() {
        return Outcome::NotApplicable(
            "reads compiler settings from a JVM system property (.env)".to_string(),
        );
    }
    let temp = common::scratch_dir().expect("allocate a scratch directory");
    let test_data = args_file.parent().unwrap();
    let invocations = match read_args(toolchain, args_file, &temp) {
        Ok(invocations) => invocations,
        Err(reason) => return Outcome::NotApplicable(reason),
    };
    let mut output = String::new();
    let mut exit = Some(0);
    for arguments in &invocations.groups {
        // JetBrains runs the corpus under UTF-8; a JVM in the POSIX locale prints `?` for `–`.
        let result = Command::new(&toolchain.compiler)
            .args(arguments)
            .current_dir(&toolchain.root)
            .env("LC_ALL", "C.UTF-8")
            .output()
            .unwrap_or_else(|error| panic!("run {}: {error}", toolchain.compiler.display()));
        output.push_str(&String::from_utf8_lossy(&result.stderr));
        exit = result.status.code();
        if exit != Some(0) {
            break;
        }
    }
    let actual = format!(
        "{}\n{}\n",
        normalize(toolchain, &output, test_data, &temp),
        exit_code_name(exit)
    );
    let expected = expected_output(toolchain, &args_file.with_extension("out"));
    let mut problems = Vec::new();
    if comparable(&actual) != comparable(&expected) {
        problems.push(output_diff(&comparable(&expected), &comparable(&actual)));
    }
    let checks = args_file.with_extension("test");
    if checks.is_file() {
        problems.extend(additional_checks(&checks, test_data, &temp));
    }
    let _ = std::fs::remove_dir_all(&temp);
    if problems.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(problems.join("\n"))
    }
}

/// The committed `.out` file with `$VERSION$` spelled as the reference release, the compiler
/// version a release build prints.
fn expected_output(toolchain: &Toolchain, out_file: &Path) -> String {
    let text = std::fs::read_to_string(out_file)
        .unwrap_or_else(|error| panic!("read {}: {error}", out_file.display()))
        .replace("$VERSION$", &toolchain.reference.to_string());
    // A x.y.0 release prints its version as the metadata version, which the actual side replaces;
    // replace it here the same way so the two meet.
    let abi = format!(
        "{}.{}.0",
        toolchain.reference.major, toolchain.reference.minor
    );
    replace_version(&text, &format!(" {abi}"), " $ABI_VERSION$")
}

/// The first differing line of each side, which is what a reviewer needs from a list of hundreds.
fn output_diff(expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let first = expected_lines
        .iter()
        .zip(&actual_lines)
        .position(|(expected, actual)| expected != actual)
        .unwrap_or(expected_lines.len().min(actual_lines.len()));
    format!(
        "line {}: expected {:?}, got {:?}",
        first + 1,
        expected_lines.get(first).copied().unwrap_or("<end>"),
        actual_lines.get(first).copied().unwrap_or("<end>"),
    )
}

/// `AbstractCliTest.doTestAdditionalChecks`: `// EXISTS:`, `// ABSENT:`, `// CONTAINS:` and
/// `// NOT_CONTAINS:` lines about the files a compilation wrote.
fn additional_checks(checks: &Path, test_data: &Path, temp: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(checks)
        .unwrap_or_else(|error| panic!("read {}: {error}", checks.display()));
    let resolve = |name: &str| match name.strip_prefix("$TESTDATA_DIR$/") {
        Some(relative) => test_data.join(relative),
        None => temp.join(name),
    };
    let mut problems = Vec::new();
    for line in text.lines() {
        if let Some(list) = line.strip_prefix("// EXISTS: ") {
            for name in list.split(',').map(str::trim) {
                if !resolve(name).is_file() {
                    problems.push(format!("File does not exist, but should: {name}"));
                }
            }
        } else if let Some(list) = line.strip_prefix("// ABSENT: ") {
            for name in list.split(',').map(str::trim) {
                if resolve(name).is_file() {
                    problems.push(format!("File exists, but shouldn't: {name}"));
                }
            }
        } else if let Some((spec, negated)) = line
            .strip_prefix("// CONTAINS: ")
            .map(|spec| (spec, false))
            .or_else(|| {
                line.strip_prefix("// NOT_CONTAINS: ")
                    .map(|spec| (spec, true))
            })
        {
            let (name, needle) = spec.split_once(',').expect("FILE, TEXT");
            let (name, needle) = (name.trim(), needle.trim());
            match std::fs::read_to_string(resolve(name)) {
                Ok(contents) if contents.contains(needle) == negated => problems.push(format!(
                    "File {name} {} string: {needle}",
                    if negated {
                        "contains"
                    } else {
                        "does not contain"
                    }
                )),
                Ok(_) => {}
                Err(_) => problems.push(format!("File does not exist: {name}")),
            }
        }
    }
    problems
}

fn run_corpus(toolchain: &Toolchain) -> BTreeMap<String, Outcome> {
    cli_cases(&toolchain.root)
        .par_iter()
        .map(|args_file| {
            (
                case_key(&toolchain.root, args_file),
                run_case(toolchain, args_file),
            )
        })
        .collect()
}

fn expectation_path(directory: &str, version: KotlinVersion) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(directory)
        .join("jvm")
        .join(format!("{version}.txt"))
}

fn load_expectation(path: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    box_ratchet::parse(&text)
        .unwrap_or_else(|error| panic!("invalid expectation {}: {error}", path.display()))
}

fn render_failures(version: KotlinVersion, failures: &BTreeSet<String>) -> String {
    let mut text = format!(
        "# Kotlin {version} compiler/testData/cli/jvm cases krusty does not reproduce yet, relative to\n\
         # that directory. Regenerate with KRUSTY_BLESS_CLI_EXPECTATIONS=1 on a full local run.\n\
         # The list only shrinks.\n"
    );
    for failure in failures {
        text.push_str(failure);
        text.push('\n');
    }
    text
}

#[test]
fn krusty_reproduces_kotlincs_cli_test_corpus() {
    let toolchain = Toolchain::new(common::krusty_binary(), None, None);
    let version = toolchain.reference;
    let outcomes = run_corpus(&toolchain);
    let not_applicable =
        load_expectation(&expectation_path("cli_expected_not_applicable", version));
    let failures_path = expectation_path("cli_expected_failures", version);

    let mut failed = BTreeMap::new();
    let mut passed = 0;
    let mut problems = Vec::new();
    for (key, outcome) in &outcomes {
        match outcome {
            Outcome::Pass if not_applicable.contains(key) => problems.push(format!(
                "{key}: passes but is listed as not applicable; remove it from the list"
            )),
            Outcome::Pass => passed += 1,
            Outcome::Fail(_) if not_applicable.contains(key) => {}
            Outcome::Fail(reason) => {
                failed.insert(key.clone(), reason.clone());
            }
            Outcome::NotApplicable(_) => {}
        }
    }
    for key in &not_applicable {
        if !outcomes.contains_key(key) {
            problems.push(format!(
                "{key}: listed as not applicable but absent from the corpus"
            ));
        }
    }
    let failing: BTreeSet<String> = failed.keys().cloned().collect();
    let bless = box_ratchet::bless_requested(
        std::env::var("KRUSTY_BLESS_CLI_EXPECTATIONS")
            .ok()
            .as_deref(),
    )
    .unwrap_or_else(|error| panic!("{}", error.replace("BOX", "CLI")));
    if bless {
        assert!(
            std::env::var_os("CI").is_none(),
            "KRUSTY_BLESS_CLI_EXPECTATIONS is refused under CI: expectations are blessed locally"
        );
        box_ratchet::write_atomic(&failures_path, &render_failures(version, &failing))
            .unwrap_or_else(|error| panic!("write {}: {error}", failures_path.display()));
    }
    let expected_failures = load_expectation(&failures_path);
    for key in failing.difference(&expected_failures) {
        problems.push(format!("{key}: newly fails: {}", failed[key]));
    }
    for key in expected_failures.difference(&failing) {
        match outcomes.get(key) {
            Some(Outcome::Pass) => problems.push(format!(
                "{key}: now passes; remove it from {}",
                failures_path.display()
            )),
            Some(Outcome::NotApplicable(reason)) => {
                eprintln!(
                    "cli corpus: {key} is listed as failing but not applicable here: {reason}"
                )
            }
            _ => problems.push(format!(
                "{key}: listed as failing but absent from the corpus"
            )),
        }
    }
    eprintln!(
        "cli corpus: {passed} passed, {} expected failures, {} not applicable, of {}",
        failing.len(),
        outcomes.len() - passed - failing.len(),
        outcomes.len()
    );
    assert!(
        passed > 0,
        "no CLI case passed: check the corpus and the krusty binary"
    );
    assert!(
        problems.is_empty(),
        "{} CLI corpus case(s) disagree with {}:\n  {}",
        problems.len(),
        failures_path.display(),
        problems.join("\n  ")
    );
}

/// Runs the reference kotlinc through the same runner. Every case the not-applicable list does not
/// excuse must pass, which proves the runner reproduces `AbstractCliTest` and that each listed case
/// is an environment difference rather than a krusty gap.
#[test]
#[ignore = "runs the reference kotlinc over the whole CLI corpus (several minutes)"]
fn the_reference_compiler_reproduces_every_applicable_cli_case() {
    let kotlinc = krusty::toolchain::kotlinc_path().expect("the reference kotlinc is provisioned");
    let home = kotlinc
        .parent()
        .and_then(Path::parent)
        .expect("kotlinc lives in <home>/bin")
        .to_path_buf();
    let toolchain = Toolchain::new(
        home.join("bin/kotlinc-jvm"),
        Some(home),
        jvm_runtime_version(),
    );
    let not_applicable = load_expectation(&expectation_path(
        "cli_expected_not_applicable",
        toolchain.reference,
    ));
    let outcomes = run_corpus(&toolchain);
    let mut problems = Vec::new();
    for (key, outcome) in &outcomes {
        match outcome {
            Outcome::Fail(reason) if !not_applicable.contains(key) => {
                problems.push(format!("{key}: {reason}"))
            }
            Outcome::Pass if not_applicable.contains(key) => problems.push(format!(
                "{key}: the reference compiler passes it here; remove it from the not-applicable list"
            )),
            _ => {}
        }
    }
    assert!(
        problems.is_empty(),
        "the reference compiler disagrees with the CLI corpus runner:\n  {}",
        problems.join("\n  ")
    );
}

/// `java.runtime.version` of the `java` on `JAVA_HOME`, which the reference `-version` line prints.
fn jvm_runtime_version() -> Option<String> {
    let java = std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/java"))
        .unwrap_or_else(|| PathBuf::from("java"));
    let output = Command::new(java)
        .args(["-XshowSettings:properties", "-version"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.trim().strip_prefix("java.runtime.version = "))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metadata_version_is_replaced_unless_a_qualifier_follows() {
        assert_eq!(
            replace_version("expected 2.4.0-dev", " 2.4.0", " $ABI_VERSION$"),
            "expected 2.4.0-dev"
        );
        assert_eq!(
            replace_version("version is 2.4.0, expected", " 2.4.0", " $ABI_VERSION$"),
            "version is $ABI_VERSION$, expected"
        );
    }

    #[test]
    fn comparison_ignores_trailing_whitespace() {
        assert_eq!(comparable("a  \nb\n\n"), comparable("a\nb"));
        assert_ne!(comparable("a\n b"), comparable("a\nb"));
    }
}
