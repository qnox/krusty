//! The frame computation reproduces the `StackMapTable` of every Kotlin method kotlinc wrote.
//!
//! kotlinc's writer computes all frames with ASM (`COMPUTE_FRAMES`, every common superclass
//! `java/lang/Object`). `krusty::jvm::frame_audit` runs krusty's port of that computation over a
//! class file and compares the result with the table the class carries. Over the reference
//! toolchain's own kotlinc-built jars, nothing may differ.

use krusty::jvm::frame_audit::{audit_jar, Outcome};

use crate::common;

/// `(methods compared, of them with frames, differences)` over every class of `jar`.
fn audit(jar: &std::path::Path) -> (usize, usize, Vec<String>) {
    let entries = audit_jar(jar).unwrap_or_else(|error| panic!("read {}: {error}", jar.display()));
    let (mut compared, mut framed, mut differences) = (0, 0, Vec::new());
    for (class, audits) in entries {
        let audits = audits.unwrap_or_else(|error| panic!("parse {class}: {error}"));
        for audit in audits {
            match audit.outcome {
                Outcome::Identical { frames } => {
                    compared += 1;
                    framed += usize::from(frames > 0);
                }
                Outcome::Different { first, .. } => {
                    differences.push(format!(
                        "{class} {}{}: {first}",
                        audit.name, audit.descriptor
                    ));
                }
                // javac's frames are not kotlinc's, and a body the class reader refuses has no
                // instructions to compute from.
                Outcome::Declined { reason } => assert!(
                    reason == "compiled from Java" || reason == "code unreadable",
                    "{class} {}{}: declined: {reason}",
                    audit.name,
                    audit.descriptor
                ),
            }
        }
    }
    (compared, framed, differences)
}

#[test]
fn computed_frames_match_every_kotlin_method_of_the_reference_jars() {
    for jar in [common::stdlib_jar(), common::coroutines_jar()] {
        let (compared, framed, differences) = audit(&jar);
        assert!(
            differences.is_empty(),
            "{} of the methods in {} differ, first: {}",
            differences.len(),
            jar.display(),
            differences[0]
        );
        // A jar that audits nothing proves nothing.
        assert!(
            compared > 1000 && framed > 500,
            "{}: only {compared} methods compared, {framed} with frames",
            jar.display()
        );
    }
}
