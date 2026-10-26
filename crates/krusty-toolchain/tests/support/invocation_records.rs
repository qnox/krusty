pub use krusty_test_support::InvocationRecords;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::TempDir;
    use std::io::ErrorKind;
    use std::path::Path;
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
    fn clearing_a_completed_lane_leaves_it_absent_until_the_next_invocation() {
        let temp = TempDir::new("invocation-record-clear");
        let records = InvocationRecords::at(&temp.0.join("compiles"));
        records.clear_after_run().unwrap();
        assert!(!records.path().exists());
        records.create().unwrap();
        success(run(&super::super::recorder(), &records, &["first"]));
        assert_eq!(records.read_snapshot().unwrap(), b"first\n----\n");
        records.clear_after_run().unwrap();
        assert!(!records.path().exists());
        records.clear_after_run().unwrap();
        assert!(!records.path().exists());
    }

    #[test]
    fn clearing_an_incomplete_lane_preserves_its_owned_evidence() {
        let temp = TempDir::new("invocation-record-clear-incomplete");
        let records = InvocationRecords::at(&temp.0.join("compiles"));
        records.create().unwrap();
        let slot = records.path().join("0");
        std::fs::create_dir(&slot).unwrap();
        std::fs::write(slot.join("owner"), b"").unwrap();
        assert_eq!(
            records.clear_after_run().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(slot.join("owner")).unwrap(), b"");
        std::fs::write(slot.join("arguments"), b"completed\n----\n").unwrap();
        records.clear_after_run().unwrap();
        assert!(!records.path().exists());
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
        std::fs::write(first.join("owner"), b"").unwrap();
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
            std::fs::write(slot.join("owner"), b"").unwrap();
            assert_eq!(
                records.read_snapshot().unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn a_complete_payload_requires_an_exclusive_owner_file() {
        for owner in ["missing", "directory", "nonempty"] {
            let temp = TempDir::new("invocation-record-owner");
            let records = InvocationRecords::new(&temp.0);
            records.create().unwrap();
            let slot = records.path().join("0");
            std::fs::create_dir(&slot).unwrap();
            let payload = b"argument\n----\n";
            std::fs::write(slot.join("arguments"), payload).unwrap();
            match owner {
                "missing" => {}
                "directory" => std::fs::create_dir(slot.join("owner")).unwrap(),
                "nonempty" => std::fs::write(slot.join("owner"), b"not a claim").unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(
                records.read_snapshot().unwrap_err().kind(),
                ErrorKind::InvalidData
            );
            assert_eq!(std::fs::read(slot.join("arguments")).unwrap(), payload);
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
