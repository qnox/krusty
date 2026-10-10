//! A command run whole, by krusty-toolchain and by `kotlin` in the same project: the exit status
//! and the bytes on stdout and on stderr must be the same, once each run's project directory is
//! replaced by one placeholder. This covers what comparing problems in process does not: argument
//! parsing, module selection, the exit status, how each problem is drawn and where it and the
//! result are written.

use std::process::Command;

use crate::support::kotlin::{self, Invocation};
use crate::support::{self, Case};

/// How each of `commands`, `(case in tests/cases/<directory>, arguments)`, run by
/// krusty-toolchain differs from the same command run by the toolchain; empty when none does.
pub fn differences(directory: &str, commands: &[(&str, &[&str])]) -> Vec<String> {
    let cases = support::cases(directory);
    let case = |name: &str| -> &Case {
        cases
            .iter()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("no case `{name}` in {directory}"))
    };
    let invocations: Vec<Invocation<'_>> = commands
        .iter()
        .map(|&(name, args)| Invocation {
            case: name,
            files: &case(name).files,
            repository: None,
            args,
        })
        .collect();
    let outputs = kotlin::kotlin_all(&invocations);
    let placed =
        |root: &str, bytes: &[u8]| String::from_utf8_lossy(bytes).replace(root, "<project>");
    let mut differences = Vec::new();
    for (&(name, args), toolchain) in commands.iter().zip(outputs) {
        let temp = support::TempDir::new(&format!("command-{directory}-{name}"));
        let root = support::materialize(&temp, &case(name).files);
        let ran = Command::new(env!("CARGO_BIN_EXE_krusty-toolchain"))
            .args(args)
            .current_dir(&root)
            .output()
            .expect("run krusty-toolchain");
        let root = root.display().to_string();
        let actual = (
            ran.status.code(),
            placed(&root, &ran.stdout),
            placed(&root, &ran.stderr),
        );
        let expected = (
            Some(toolchain.code),
            placed(&toolchain.root, &toolchain.stdout),
            placed(&toolchain.root, &toolchain.stderr),
        );
        if actual != expected {
            differences.push(format!(
                "{name} {args:?}: krusty-toolchain exited {:?} with stdout\n{:?}\nstderr\n{:?}\nthe toolchain exited {:?} with stdout\n{:?}\nstderr\n{:?}",
                actual.0, actual.1, actual.2, expected.0, expected.1, expected.2
            ));
        }
    }
    differences
}
