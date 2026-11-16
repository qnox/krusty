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
use std::sync::OnceLock;

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
    /// The JDK a JVM compiler runs on, and its `java.runtime.version` for the reference
    /// compiler's `-version` line.
    java_home: Option<PathBuf>,
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
    fn new(compiler: PathBuf, home: Option<PathBuf>) -> Self {
        let dist_jars = DIST_JARS
            .iter()
            .filter_map(|(placeholder, name)| {
                krusty::toolchain::dist_jar(name).map(|jar| (*placeholder, jar))
            })
            .collect();
        let root = super::box_inventory::kotlin_checkout("compiler/testData/cli/jvm");
        link_dist(&root);
        Toolchain {
            compiler,
            root,
            reference: krusty::kotlin_version::target(),
            jdks: available_jdks(),
            dist_jars,
            home,
            java_home: None,
            jvm_version: None,
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

/// The JDK feature releases the corpus names (`$JDK_1_8`, `$JDK_11_0`, `$JDK_17$`, `$JDK_21`).
const CORPUS_JDKS: [u32; 4] = [8, 11, 17, 21];

/// JDK homes by feature release. Installed JDKs are found where the platform puts them
/// (`/usr/lib/jvm`, `/Library/Java/JavaVirtualMachines`), then `JAVA_HOME`, then the
/// `JAVA_HOME_<release>_X64` variables `actions/setup-java` exports, then `KRUSTY_JDK_<release>_HOME`;
/// a later source overrides an earlier one. Each home's own `release` file says its release.
fn available_jdks() -> BTreeMap<u32, PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    for parent in ["/usr/lib/jvm", "/Library/Java/JavaVirtualMachines"] {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let mut found: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .map(|path| {
                    let mac = path.join("Contents/Home");
                    if mac.is_dir() {
                        mac
                    } else {
                        path
                    }
                })
                .collect();
            found.sort();
            homes.extend(found);
        }
    }
    homes.extend(std::env::var_os("JAVA_HOME").map(PathBuf::from));
    for release in CORPUS_JDKS {
        for variable in [
            format!("JAVA_HOME_{release}_X64"),
            format!("JAVA_HOME_{release}_ARM64"),
            format!("KRUSTY_JDK_{release}_HOME"),
        ] {
            homes.extend(std::env::var_os(variable).map(PathBuf::from));
        }
    }
    let mut jdks = BTreeMap::new();
    for home in homes {
        if let Some(release) = jdk_release(&home) {
            jdks.insert(release, home);
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
    /// The case exercises something no released compiler does, whatever machine runs it; the
    /// not-applicable list must name it.
    Excluded(String),
    /// The case needs something this machine does not supply, such as a JDK. That is always an
    /// error: a gate that quietly ran fewer cases would stay green while testing less.
    Unavailable(String),
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

fn run_case(toolchain: &Toolchain, args_file: &Path, recording: Option<&Recording>) -> Outcome {
    if args_file.with_extension("env").exists() {
        // `AbstractCliTest` passes the `.env` text through `kotlin.language.settings`, which
        // `CLICompiler` reads only when its class path carries
        // `LanguageVersionSettings.RESOURCE_NAME_TO_ALLOW_READING_FROM_ENVIRONMENT`. A release
        // dist does not, so neither kotlinc nor krusty is expected to read it.
        return Outcome::Excluded(
            "reads compiler settings from the environment, which a release compiler never does"
                .to_string(),
        );
    }
    let temp = common::scratch_dir().expect("allocate a scratch directory");
    let outcome = match read_args(toolchain, args_file, &temp) {
        Ok(invocations) => match recording {
            None => evaluate(
                toolchain,
                args_file,
                &temp,
                &invoke(toolchain, &invocations),
            ),
            Some(recording) => recording.run(toolchain, args_file, &temp, &invocations),
        },
        Err(reason) => Outcome::Unavailable(reason),
    };
    let _ = std::fs::remove_dir_all(&temp);
    outcome
}

/// What a case's invocations wrote to stderr, and the exit code of the last one that ran.
struct Observation {
    stderr: String,
    exit: Option<i32>,
}

fn invoke(toolchain: &Toolchain, invocations: &Invocations) -> Observation {
    let mut stderr = String::new();
    let mut exit = Some(0);
    for arguments in &invocations.groups {
        // JetBrains runs the corpus under UTF-8; a JVM in the POSIX locale prints `?` for `–`.
        let mut command = Command::new(&toolchain.compiler);
        command
            .args(arguments)
            .current_dir(&toolchain.root)
            .env("LC_ALL", "C.UTF-8");
        if let Some(java_home) = &toolchain.java_home {
            command
                .env("JAVA_HOME", java_home)
                .env("JAVA_OPTS", reference_jvm_options())
                .env_remove("JAVACMD");
        }
        let result = command
            .output()
            .unwrap_or_else(|error| panic!("run {}: {error}", toolchain.compiler.display()));
        stderr.push_str(&String::from_utf8_lossy(&result.stderr));
        exit = result.status.code();
        if exit != Some(0) {
            break;
        }
    }
    Observation { stderr, exit }
}

/// Compares an observation with the case's `.out` file and runs its `.test` checks against the
/// files left in `temp`.
fn evaluate(
    toolchain: &Toolchain,
    args_file: &Path,
    temp: &Path,
    observation: &Observation,
) -> Outcome {
    let test_data = args_file.parent().unwrap();
    let actual = format!(
        "{}\n{}\n",
        normalize(toolchain, &observation.stderr, test_data, temp),
        exit_code_name(observation.exit)
    );
    let expected = expected_output(toolchain, &args_file.with_extension("out"));
    let mut problems = Vec::new();
    if comparable(&actual) != comparable(&expected) {
        problems.push(output_diff(&comparable(&expected), &comparable(&actual)));
    }
    let checks = args_file.with_extension("test");
    if checks.is_file() {
        problems.extend(additional_checks(&checks, test_data, temp));
    }
    if problems.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(problems.join("\n"))
    }
}

/// The reference compiler's observations, recorded in the class-dump archive
/// ([`common::byte_dump::reference_files`]) the byte-equality and diagnostics tests replay.
///
/// Only kotlinc's side is recorded: what it printed, its exit code, and for a case with `.test`
/// checks the files it wrote. Every run still normalizes the observation, compares it with the
/// `.out` file and runs the checks, so the runner is tested on every run while kotlinc runs only for
/// a case whose inputs changed. The key covers the case's expanded invocations, the whole CLI
/// corpus, the kotlinc distribution, every JDK the corpus names, and the JVM options; the archive
/// entry is also bound to the exact compiler version.
///
/// Only a passing observation of a case the not-applicable list does not name is recorded. A listed
/// case is expected to fail, and a transient kotlinc failure or out-of-memory exit would look the
/// same, so the few listed cases always run kotlinc; a failing run of any other case is retried by
/// the next run instead of being replayed.
struct Recording<'a> {
    not_applicable: &'a BTreeSet<String>,
}

/// Bumped when what an entry records, or how it is keyed, changes.
const CLI_REFERENCE_SALT: &str = "cli-reference-v1";

/// Stands for the case's scratch directory in a recorded stderr, which a replay runs in a new one.
const RECORDED_TEMP_DIR: &str = "\0TEMP_DIR\0";

/// Where a recorded file the compilation wrote is filed: `temp/<path under the scratch directory>`.
const RECORDED_FILE_PREFIX: &str = "temp/";

impl Recording<'_> {
    fn run(
        &self,
        toolchain: &Toolchain,
        args_file: &Path,
        temp: &Path,
        invocations: &Invocations,
    ) -> Outcome {
        let key = case_key(&toolchain.root, args_file);
        if self.not_applicable.contains(&key) {
            return evaluate(toolchain, args_file, temp, &invoke(toolchain, invocations));
        }
        let fingerprint = reference_fingerprint(toolchain, &key, temp, invocations);
        let mut live = None;
        let recorded = common::byte_dump::reference_files(
            &format!("cli-reference-{key}"),
            fingerprint,
            || {
                let observation = invoke(toolchain, invocations);
                let files = record(&observation, args_file, temp);
                let outcome = evaluate(toolchain, args_file, temp, &observation);
                let passed = outcome == Outcome::Pass;
                live = Some(outcome);
                passed.then_some(files)
            },
        );
        if let Some(outcome) = live {
            return outcome;
        }
        let files = recorded.expect("a recorded observation or a live run");
        let observation = replay(&files, temp)
            .unwrap_or_else(|error| panic!("{key}: the recorded kotlinc observation {error}"));
        evaluate(toolchain, args_file, temp, &observation)
    }
}

fn record(observation: &Observation, args_file: &Path, temp: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    files.insert(
        "stderr".to_string(),
        observation
            .stderr
            .replace(&temp.display().to_string(), RECORDED_TEMP_DIR)
            .into_bytes(),
    );
    if let Some(code) = observation.exit {
        files.insert("exit".to_string(), code.to_string().into_bytes());
    }
    // Only the `.test` checks read the files a compilation wrote.
    if args_file.with_extension("test").is_file() {
        let mut stack = vec![temp.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
            for entry in entries {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    let relative = path
                        .strip_prefix(temp)
                        .expect("a file under the scratch dir");
                    let bytes = std::fs::read(&path)
                        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                    files.insert(
                        format!("{RECORDED_FILE_PREFIX}{}", relative.display()),
                        bytes,
                    );
                }
            }
        }
    }
    files
}

/// Restores a recorded observation into this run's scratch directory. A file this run already
/// wrote, such as an expanded argument file naming this scratch directory, is kept.
fn replay(files: &BTreeMap<String, Vec<u8>>, temp: &Path) -> Result<Observation, String> {
    let stderr = files.get("stderr").ok_or("has no stderr")?;
    let stderr = String::from_utf8(stderr.clone())
        .map_err(|_| "has a stderr that is not UTF-8".to_string())?
        .replace(RECORDED_TEMP_DIR, &temp.display().to_string());
    let exit = files.get("exit").ok_or("has no exit code")?;
    let exit = std::str::from_utf8(exit)
        .ok()
        .and_then(|code| code.parse().ok())
        .ok_or("has an exit code that is not a number")?;
    for (name, bytes) in files {
        let Some(relative) = name.strip_prefix(RECORDED_FILE_PREFIX) else {
            continue;
        };
        let path = temp.join(relative);
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("create {}: {error}", parent.display()));
        }
        std::fs::write(&path, bytes)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    }
    Ok(Observation {
        stderr,
        exit: Some(exit),
    })
}

/// JVM options a launched kotlinc picks up from the environment it inherits.
const INHERITED_JVM_OPTION_VARIABLES: [&str; 3] =
    ["JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS"];

/// A JDK's class library: the modules image, or JDK 8's `rt.jar`, which has none.
fn jdk_class_library(home: &Path) -> Option<PathBuf> {
    ["lib/modules", "jre/lib/rt.jar"]
        .iter()
        .map(|path| home.join(path))
        .find(|path| path.is_file())
}

/// The recording key of one reference case. The scratch directory differs on every run, so the
/// invocations spell it as `$TEMP_DIR$`.
fn reference_fingerprint(
    toolchain: &Toolchain,
    key: &str,
    temp: &Path,
    invocations: &Invocations,
) -> u128 {
    let temp = temp.display().to_string();
    let mut parts: Vec<Vec<u8>> = vec![
        CLI_REFERENCE_SALT.into(),
        key.into(),
        reference_environment(toolchain).as_bytes().to_vec(),
        reference_jvm_options().into_bytes(),
    ];
    // The JVM reads these from the inherited environment on top of `JAVA_OPTS`.
    for variable in INHERITED_JVM_OPTION_VARIABLES {
        parts.push(variable.as_bytes().to_vec());
        parts.push(
            std::env::var_os(variable)
                .map(|value| value.as_encoded_bytes().to_vec())
                .unwrap_or_default(),
        );
    }
    for group in &invocations.groups {
        parts.push((group.len() as u64).to_le_bytes().to_vec());
        parts.extend(
            group
                .iter()
                .map(|argument| argument.replace(&temp, "$TEMP_DIR$").into_bytes()),
        );
    }
    let parts: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    common::byte_dump::fingerprint_parts(&parts)
}

/// Everything a reference case reads besides its own arguments, identified once per process: the
/// CLI corpus and the checkout's third-party jars it names, the kotlinc distribution with its
/// launchers, the JDK it runs on, and the class library of each JDK the corpus names, all by
/// content, with the paths that appear in the output.
fn reference_environment(toolchain: &Toolchain) -> &'static str {
    static ENVIRONMENT: OnceLock<String> = OnceLock::new();
    ENVIRONMENT.get_or_init(|| {
        let java_home = toolchain
            .java_home
            .as_deref()
            .expect("the reference compiler has a JDK");
        let mut inputs: Vec<PathBuf> = ["compiler/testData/cli", "third-party"]
            .iter()
            .map(|path| toolchain.root.join(path))
            .filter(|path| path.is_dir())
            .collect();
        // The distribution identity covers `lib`; the launcher scripts the cases run live in `bin`.
        let home = toolchain
            .home
            .as_deref()
            .expect("the reference compiler has a home");
        inputs.push(home.join("bin"));
        inputs.extend(toolchain.jdks.iter().map(|(release, home)| {
            jdk_class_library(home).unwrap_or_else(|| {
                panic!(
                    "JDK {release} at {} has neither lib/modules nor jre/lib/rt.jar",
                    home.display()
                )
            })
        }));
        let content = common::byte_dump::immutable_classpath_content_fingerprint(
            &inputs,
            &java_home.join("lib/modules"),
        )
        .unwrap_or_else(|error| panic!("identify the reference CLI environment: {error}"));
        let mut rows = vec![
            content,
            format!("root:{}", toolchain.root.display()),
            format!("compiler:{}", toolchain.compiler.display()),
            format!("java:{}", java_home.display()),
            format!("jvm:{}", toolchain.jvm_version.as_deref().unwrap_or("")),
        ];
        // The class libraries above cover a JDK changed in place; the `release` file, which
        // [`jdk_release`] already required, and the home name the update in cache inspection.
        for (release, home) in &toolchain.jdks {
            let release_file = home.join("release");
            let identity = std::fs::read(&release_file)
                .unwrap_or_else(|error| panic!("read {}: {error}", release_file.display()));
            rows.push(format!(
                "jdk{release}:{}:{}",
                home.display(),
                String::from_utf8_lossy(&identity)
            ));
        }
        rows.join("\n")
    })
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

fn run_corpus(toolchain: &Toolchain, recording: Option<&Recording>) -> BTreeMap<String, Outcome> {
    cli_cases(&toolchain.root)
        .par_iter()
        .map(|args_file| {
            (
                case_key(&toolchain.root, args_file),
                run_case(toolchain, args_file, recording),
            )
        })
        .collect()
}

fn expectation_path(directory: &str, version: KotlinVersion) -> PathBuf {
    box_ratchet::read_path(directory, "jvm", version)
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
    let toolchain = Toolchain::new(common::krusty_binary(), None);
    let version = toolchain.reference;
    let outcomes = run_corpus(&toolchain, None);
    let not_applicable =
        load_expectation(&expectation_path("cli_expected_not_applicable", version));

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
            Outcome::Excluded(_) if not_applicable.contains(key) => {}
            Outcome::Excluded(reason) => problems.push(format!(
                "{key}: {reason}; add it to the not-applicable list with that reason"
            )),
            Outcome::Unavailable(reason) => {
                problems.push(format!("{key}: cannot run here: {reason}"))
            }
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
        box_ratchet::record(
            "cli_expected_failures",
            "jvm",
            version,
            &failing,
            &render_failures(version, &failing),
        )
        .unwrap_or_else(|error| panic!("record the {version} CLI expectations: {error}"));
    }
    let failures_path = expectation_path("cli_expected_failures", version);
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
            // Already reported above as a case that could not run.
            Some(Outcome::Unavailable(_)) => {}
            Some(Outcome::Excluded(reason)) => problems.push(format!(
                "{key}: listed as failing but excluded ({reason}); remove it from {}",
                failures_path.display()
            )),
            _ => problems.push(format!(
                "{key}: listed as failing but absent from the corpus"
            )),
        }
    }
    eprintln!(
        "cli corpus: {passed} passed, {} expected failures, {} listed as not applicable, of {}",
        failing.len(),
        not_applicable.len(),
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
/// excuse must pass and every listed one must not, which checks that the runner reproduces
/// `AbstractCliTest` and that each listed case is an environment difference rather than a krusty
/// gap. kotlinc's observations replay from the recorded-bytes archive ([`Recording`]). It is ignored
/// in ordinary runs because a cold archive takes minutes; the conformance job runs it for every
/// supported release (`scripts/conformance-regressions.sh`).
#[test]
#[ignore = "runs the reference kotlinc over the whole CLI corpus (minutes without its recordings)"]
fn the_reference_compiler_reproduces_every_applicable_cli_case() {
    let kotlinc = krusty::toolchain::kotlinc_path().expect("the reference kotlinc is provisioned");
    let home = kotlinc
        .parent()
        .and_then(Path::parent)
        .expect("kotlinc lives in <home>/bin")
        .to_path_buf();
    let mut toolchain = Toolchain::new(home.join("bin/kotlinc-jvm"), Some(home));
    let java_home = toolchain
        .jdks
        .get(&REFERENCE_JDK)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "the reference compiler runs on JDK {REFERENCE_JDK} \
                 (set KRUSTY_JDK_{REFERENCE_JDK}_HOME)"
            )
        });
    toolchain.jvm_version = jvm_runtime_version(&java_home);
    toolchain.java_home = Some(java_home);
    let not_applicable = load_expectation(&expectation_path(
        "cli_expected_not_applicable",
        toolchain.reference,
    ));
    let outcomes = run_corpus(
        &toolchain,
        Some(&Recording {
            not_applicable: &not_applicable,
        }),
    );
    let mut problems = Vec::new();
    for (key, outcome) in &outcomes {
        match outcome {
            Outcome::Fail(reason) if !not_applicable.contains(key) => {
                problems.push(format!("{key}: {reason}"))
            }
            Outcome::Pass if not_applicable.contains(key) => problems.push(format!(
                "{key}: the reference compiler passes it here; remove it from the not-applicable list"
            )),
            Outcome::Excluded(reason) if !not_applicable.contains(key) => problems.push(format!(
                "{key}: {reason}; add it to the not-applicable list with that reason"
            )),
            Outcome::Unavailable(reason) => problems.push(format!("{key}: cannot run here: {reason}")),
            _ => {}
        }
    }
    for key in &not_applicable {
        if !outcomes.contains_key(key) {
            problems.push(format!(
                "{key}: listed as not applicable but absent from the corpus"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the reference compiler disagrees with the CLI corpus runner:\n  {}",
        problems.join("\n  ")
    );
}

/// The JDK the reference kotlinc runs on: the newest release the corpus names. On JDK 25, kotlinc
/// runs out of memory on `argFileCommonChecks/argfileWithEmptyArgument` instead of reporting that
/// there are no sources, apparently reading the empty argument as the working directory and
/// compiling the whole checkout.
const REFERENCE_JDK: u32 = 21;

/// The JIT option every reference compile adds. Each case starts a fresh kotlinc that compiles a
/// few small files and exits, and four run at once, so the C2 compiler's background compilation of
/// kotlinc competed for the same cores without paying off before the JVM exited. Stopping at C1
/// took the whole corpus from 588 s to 415 s with the same outcome for every case. It changes only
/// how fast the JVM runs kotlinc, never what kotlinc prints.
const REFERENCE_JIT_OPTION: &str = "-XX:TieredStopAtLevel=1";

/// The launcher applies its own heap bounds only when `JAVA_OPTS` is empty, so they are spelled out
/// here to keep every reference compile under the limits a plain `kotlinc` run has.
const KOTLINC_LAUNCHER_HEAP: &str = "-Xmx512M -Xms128M";

/// `JAVA_OPTS` for a reference compile: the caller's options, or the launcher's default heap when
/// there are none, followed by [`REFERENCE_JIT_OPTION`].
fn reference_jvm_options() -> String {
    let base = std::env::var("JAVA_OPTS")
        .ok()
        .filter(|options| !options.trim().is_empty())
        .unwrap_or_else(|| KOTLINC_LAUNCHER_HEAP.to_string());
    format!("{base} {REFERENCE_JIT_OPTION}")
}

/// `java.runtime.version` of a JDK, which the reference `-version` line prints.
fn jvm_runtime_version(java_home: &Path) -> Option<String> {
    let output = Command::new(java_home.join("bin/java"))
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

    #[test]
    fn a_recorded_observation_replays_into_a_new_scratch_directory() {
        let recorded_in = common::scratch_dir().expect("scratch directory");
        let args_file = recorded_in.join("case.args");
        std::fs::write(recorded_in.join("case.test"), "// EXISTS: out/A.class").unwrap();
        std::fs::create_dir_all(recorded_in.join("out")).unwrap();
        std::fs::write(recorded_in.join("out/A.class"), b"class").unwrap();
        std::fs::write(recorded_in.join("case.args.expanded"), "old").unwrap();
        let observation = Observation {
            stderr: format!("error: {}/out is odd\n", recorded_in.display()),
            exit: Some(1),
        };
        let files = record(&observation, &args_file, &recorded_in);

        let replayed_in = common::scratch_dir().expect("scratch directory");
        std::fs::write(replayed_in.join("case.args.expanded"), "new").unwrap();
        let replayed = replay(&files, &replayed_in).expect("a complete recording");
        assert_eq!(
            replayed.stderr,
            format!("error: {}/out is odd\n", replayed_in.display())
        );
        assert_eq!(replayed.exit, Some(1));
        assert_eq!(
            std::fs::read(replayed_in.join("out/A.class")).unwrap(),
            b"class"
        );
        assert_eq!(
            std::fs::read_to_string(replayed_in.join("case.args.expanded")).unwrap(),
            "new"
        );
        let _ = std::fs::remove_dir_all(&recorded_in);
        let _ = std::fs::remove_dir_all(&replayed_in);
    }

    #[test]
    fn a_case_without_checks_records_only_its_output() {
        let temp = common::scratch_dir().expect("scratch directory");
        std::fs::write(temp.join("A.class"), b"class").unwrap();
        let observation = Observation {
            stderr: "warning: w\n".to_string(),
            exit: Some(0),
        };
        let files = record(&observation, &temp.join("case.args"), &temp);
        assert_eq!(files.keys().collect::<Vec<_>>(), vec!["exit", "stderr"]);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn a_recording_without_an_exit_code_is_refused() {
        let temp = common::scratch_dir().expect("scratch directory");
        let files = BTreeMap::from([("stderr".to_string(), Vec::new())]);
        assert_eq!(
            replay(&files, &temp).err().as_deref(),
            Some("has no exit code")
        );
        let _ = std::fs::remove_dir_all(&temp);
    }
}
