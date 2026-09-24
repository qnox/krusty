//! Compare the `StackMapTable`s in class files with the frames kotlinc's writer computes for the same
//! instructions (ASM `COMPUTE_FRAMES`, every common superclass `java/lang/Object`).
//!
//! Over kotlinc's own output every method should come out identical; over krusty's it shows where the
//! frames recorded during emission differ from what the instructions imply.
//!
//! Usage: `cargo run --profile gate --bin framecheck -- [--list <file>] <class file, jar or dir>...`
//!
//! Prints the method counts per outcome and the most frequent kinds of difference. `--list` writes
//! one tab-separated line per non-identical method (`outcome`, `class file`, `method`, `detail`).

use krusty::jvm::frame_audit::{audit_class, audit_jar, ClassAudit, Outcome};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for entry in entries {
            collect(&entry, out);
        }
    } else if path
        .extension()
        .is_some_and(|ext| ext == "class" || ext == "jar")
    {
        out.push(path.to_path_buf());
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut list: Option<PathBuf> = None;
    let mut files = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--list" {
            list = args.next().map(PathBuf::from);
        } else {
            collect(Path::new(&arg), &mut files);
        }
    }
    let mut listing = list.map(|path| {
        std::io::BufWriter::new(std::fs::File::create(&path).expect("create the --list file"))
    });
    let (mut identical, mut different, mut declined, mut unparsed, mut framed) = (0, 0, 0, 0, 0);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut classes = 0;
    for file in &files {
        let entries: Vec<(String, ClassAudit)> = if file.extension().is_some_and(|ext| ext == "jar")
        {
            match audit_jar(file) {
                Ok(entries) => entries
                    .into_iter()
                    .map(|(name, audits)| (format!("{}!{name}", file.display()), audits))
                    .collect(),
                Err(_) => {
                    unparsed += 1;
                    continue;
                }
            }
        } else {
            match std::fs::read(file) {
                Ok(bytes) => vec![(file.display().to_string(), audit_class(&bytes))],
                Err(error) => vec![(file.display().to_string(), Err(error.to_string()))],
            }
        };
        for (class, audits) in entries {
            classes += 1;
            let Ok(audits) = audits else {
                unparsed += 1;
                continue;
            };
            for audit in audits {
                let (label, detail) = match &audit.outcome {
                    Outcome::Identical { frames } => {
                        identical += 1;
                        framed += usize::from(*frames > 0);
                        continue;
                    }
                    Outcome::Different { kind, first } => {
                        different += 1;
                        *kinds.entry(kind.describe().to_string()).or_default() += 1;
                        (kind.describe(), first.as_str())
                    }
                    Outcome::Declined { reason } => {
                        declined += 1;
                        *kinds.entry(format!("declined: {reason}")).or_default() += 1;
                        ("declined", reason.as_str())
                    }
                };
                if let Some(listing) = listing.as_mut() {
                    writeln!(
                        listing,
                        "{label}\t{class}\t{}{}\t{detail}",
                        audit.name, audit.descriptor
                    )
                    .expect("write the --list file");
                }
            }
        }
    }
    println!("classes: {classes} ({unparsed} unparsed)");
    println!("methods identical: {identical} ({framed} with frames)");
    println!("methods different: {different}");
    println!("methods declined:  {declined}");
    let mut kinds: Vec<(String, usize)> = kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (kind, count) in kinds.iter().take(25) {
        println!("{count:>8}  {kind}");
    }
}
