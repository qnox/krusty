//! Invocation-owned records, observed only after the task has drained its child processes.
//!
//! Exclusive creation assigns a numeric slot before writing. Slots stay claimed until the
//! completed run is consumed, so a later writer cannot acquire an earlier writer's ownership.
//! Numeric claim order preserves dependency/KSP order without depending on completion order.

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

pub struct InvocationRecords {
    directory: PathBuf,
}

impl InvocationRecords {
    pub fn new(run: &Path) -> Self {
        Self {
            directory: run.join("record"),
        }
    }

    pub fn create(&self) -> io::Result<()> {
        std::fs::create_dir(&self.directory)
    }

    pub fn path(&self) -> &Path {
        &self.directory
    }

    /// A non-consuming snapshot, including earlier completed runs (KSP avoidance uses this).
    /// Missing, incomplete or malformed slots are errors, never an empty successful record.
    pub fn read_snapshot(&self) -> io::Result<Vec<u8>> {
        let mut slots = Vec::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let index = name.to_str().and_then(|name| name.parse::<usize>().ok());
            let Some(index) = index.filter(|index| name == index.to_string().as_str()) else {
                return Err(self.invalid("non-numeric invocation slot"));
            };
            if !entry.file_type()?.is_dir() {
                return Err(self.invalid("invocation slot is not a directory"));
            }
            slots.push(index);
        }
        slots.sort_unstable();
        let mut record = Vec::new();
        for (expected, index) in slots.into_iter().enumerate() {
            if index != expected {
                return Err(self.invalid("missing invocation slot"));
            }
            let slot = self.directory.join(index.to_string());
            let files = std::fs::read_dir(&slot)?.collect::<io::Result<Vec<_>>>()?;
            if files.len() != 1
                || files[0].file_name() != "arguments"
                || !files[0].file_type()?.is_file()
            {
                return Err(self.invalid("incomplete invocation slot"));
            }
            let arguments = std::fs::read(slot.join("arguments"))?;
            if !arguments.ends_with(b"----\n") {
                return Err(self.invalid("unterminated invocation record"));
            }
            record.extend(arguments);
        }
        Ok(record)
    }

    /// Call only after all children have terminated. Validate before deleting any evidence.
    pub fn consume_after_run(&self) -> io::Result<Vec<u8>> {
        let record = self.read_snapshot()?;
        std::fs::remove_dir_all(&self.directory)?;
        self.create()?;
        Ok(record)
    }

    fn invalid(&self, message: &str) -> io::Error {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("{}: {message}", self.directory.display()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::TempDir;
    use std::process::{Command, Output};

    fn run(recorder: &Path, records: &InvocationRecords, arguments: &[&str]) -> Output {
        Command::new(recorder)
            .args(arguments)
            .env("KRUSTY_RECORD", records.path())
            .output()
            .unwrap()
    }

    fn success(output: Output) {
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"");
    }

    #[test]
    fn concurrent_recorders_preserve_every_complete_argument_payload() {
        let temp = TempDir::new("invocation-record-concurrent");
        let records = InvocationRecords::new(&temp.0);
        records.create().unwrap();
        let recorder = super::super::recorder();
        let payloads: Vec<_> = (0..12)
            .map(|index| format!("-module-name=module-{index}\n{}\n----\n", "x".repeat(32768)))
            .collect();
        std::thread::scope(|scope| {
            for payload in &payloads {
                let arguments: Vec<_> = payload.lines().take(2).collect();
                let records = &records;
                let recorder = &recorder;
                scope.spawn(move || success(run(recorder, records, &arguments)));
            }
        });
        let record = String::from_utf8(records.read_snapshot().unwrap()).unwrap();
        let mut found: Vec<_> = record
            .split_inclusive("----\n")
            .map(str::to_owned)
            .collect();
        let mut expected = payloads;
        // Independent claims have no required relative order; compare their complete payloads.
        found.sort();
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn snapshots_are_cumulative_and_consumption_separates_completed_runs() {
        let temp = TempDir::new("invocation-record-snapshot");
        let records = InvocationRecords::new(&temp.0);
        records.create().unwrap();
        let recorder = super::super::recorder();
        success(run(&recorder, &records, &["-module-name=first"]));
        assert_eq!(
            records.read_snapshot().unwrap(),
            b"-module-name=first\n----\n"
        );
        assert_eq!(
            records.read_snapshot().unwrap(),
            b"-module-name=first\n----\n"
        );
        success(run(&recorder, &records, &["-module-name=second"]));
        assert_eq!(
            records.consume_after_run().unwrap(),
            b"-module-name=first\n----\n-module-name=second\n----\n"
        );
        assert_eq!(records.consume_after_run().unwrap(), b"");
        success(run(&recorder, &records, &["-module-name=third"]));
        assert_eq!(
            records.read_snapshot().unwrap(),
            b"-module-name=third\n----\n"
        );
    }

    #[test]
    fn incomplete_claim_is_retained_and_later_completion_keeps_claim_order() {
        let temp = TempDir::new("invocation-record-order");
        let records = InvocationRecords::new(&temp.0);
        records.create().unwrap();
        let first = records.path().join("0");
        std::fs::create_dir(&first).unwrap();
        success(run(
            &super::super::recorder(),
            &records,
            &["-module-name=second"],
        ));
        assert_eq!(
            records.consume_after_run().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert!(first.is_dir());
        std::fs::write(first.join("arguments"), b"-module-name=first\n----\n").unwrap();
        assert_eq!(
            records.read_snapshot().unwrap(),
            b"-module-name=first\n----\n-module-name=second\n----\n"
        );
    }

    #[test]
    fn missing_and_malformed_slots_are_not_silently_ignored() {
        for (name, payload) in [
            ("1", "argument\n----\n"),
            ("01", "argument\n----\n"),
            ("unexpected", "argument\n----\n"),
            ("0", "argument\n"),
        ] {
            let temp = TempDir::new("invocation-record-invalid");
            let records = InvocationRecords::new(&temp.0);
            records.create().unwrap();
            let slot = records.path().join(name);
            std::fs::create_dir(&slot).unwrap();
            std::fs::write(slot.join("arguments"), payload).unwrap();
            assert_eq!(
                records.read_snapshot().unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn compiler_plugin_and_ksp_recorders_keep_exact_argument_bytes() {
        let temp = TempDir::new("invocation-record-arguments");
        let records = InvocationRecords::new(&temp.0);
        records.create().unwrap();
        let scripts = super::super::recorders();
        for name in [
            "krusty",
            "krusty-compiling-plugin",
            "krusty-toolchain-krusty",
        ] {
            success(run(
                &scripts.join(name),
                &records,
                &["-module-name=example", "a path with spaces", "", "$literal"],
            ));
        }
        let arguments = temp.0.join("arguments");
        std::fs::write(&arguments, b"a path with spaces\n$literal").unwrap();
        success(run(
            &scripts.join("java"),
            &records,
            &["KSPMain", &format!("@{}", arguments.display())],
        ));
        assert_eq!(
            String::from_utf8(records.read_snapshot().unwrap()).unwrap(),
            format!(
                "{}KSPMain\na path with spaces\n$literal\n----\n",
                "-module-name=example\na path with spaces\n\n$literal\n----\n".repeat(3)
            )
        );
    }

    #[test]
    fn fingerprint_requests_do_not_claim_a_slot() {
        let temp = TempDir::new("invocation-record-fingerprints");
        let records = InvocationRecords::new(&temp.0);
        records.create().unwrap();
        for recorder in [super::super::recorder(), super::super::toolchain_recorder()] {
            let output = run(&recorder, &records, &["compilation-fingerprint"]);
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(output.stdout, b"");
            assert_eq!(output.stderr, b"");
        }
        assert_eq!(records.read_snapshot().unwrap(), b"");
    }

    #[test]
    fn a_failed_claim_is_an_infrastructure_error_instead_of_a_missing_invocation() {
        let temp = TempDir::new("invocation-record-failed-claim");
        let records = InvocationRecords::new(&temp.0);
        records.create().unwrap();
        let obstruction = records.path().join("0");
        std::fs::write(&obstruction, b"occupied by a file").unwrap();
        let output = run(
            &super::super::recorder(),
            &records,
            &["-module-name=example"],
        );
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "recorder: cannot claim invocation slot {}\n",
                obstruction.display()
            )
        );
        assert_eq!(std::fs::read(obstruction).unwrap(), b"occupied by a file");
        assert_eq!(
            records.read_snapshot().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}
