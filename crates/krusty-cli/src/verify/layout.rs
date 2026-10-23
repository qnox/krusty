//! Where `krusty verify-compile` may write. kotlinc's output is what the build ships, so krusty's
//! output and the record must stay out of it, and it out of them, however the paths are spelled.

use std::path::{Component, Path, PathBuf};

/// Refuse a layout in which compiling or recording could overwrite kotlinc's output, or in which
/// the comparison would read krusty's own output as kotlinc's: a jar for either output, and any
/// two of the reference, the destination and the record that are the same path or one inside the
/// other, after resolving symbolic links and `..`. A request that compiles nothing has no
/// destination.
pub fn check_layout(
    reference: &Path,
    destination: Option<&Path>,
    record: &Path,
) -> Result<(), String> {
    if reference.is_file() {
        return Err(format!(
            "--reference {} is a file; verify-compile compares class directories",
            reference.display()
        ));
    }
    if let Some(destination) = destination.filter(|destination| {
        destination.is_file()
            || destination
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
    }) {
        return Err(format!(
            "-d {} names a jar; verify-compile compares class directories",
            destination.display()
        ));
    }
    let reference = ("--reference", reference);
    let record = ("--record", record);
    let mut pairs = Vec::new();
    if let Some(destination) = destination {
        pairs.push((("-d", destination), reference));
    }
    pairs.push((record, reference));
    if let Some(destination) = destination {
        pairs.push((record, ("-d", destination)));
    }
    for ((one, one_path), (other, other_path)) in pairs {
        let (one_resolved, other_resolved) = (resolved(one_path), resolved(other_path));
        if one_resolved.starts_with(&other_resolved) || other_resolved.starts_with(&one_resolved) {
            return Err(format!(
                "{one} {} overlaps {other} {}; kotlinc's output, krusty's output and the record must be separate",
                one_path.display(),
                other_path.display(),
            ));
        }
    }
    Ok(())
}

/// Refuse a record file that is one of the report files `verify-report` replaces in `out`, under
/// any spelling or link: it would be removed before it is read.
pub fn check_report_layout(out: &Path, records: &[PathBuf]) -> Result<(), String> {
    let reports = super::REPORT_FILES.map(|name| resolved(&out.join(name)));
    for record in records {
        if reports.contains(&resolved(record)) {
            return Err(format!(
                "record {} is a report verify-report writes to --out {}",
                record.display(),
                out.display()
            ));
        }
    }
    Ok(())
}

/// `path` made absolute and resolved component by component, as the file system will once the
/// missing directories are created: each existing prefix is canonicalized (following a symbolic
/// link wherever one appears, including after a `..`), and a missing component stays lexical,
/// since krusty creates it as a plain directory.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::env::current_dir()
        .map(|directory| directory.join(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other.as_os_str());
                if let Ok(canonical) = resolved.canonicalize() {
                    resolved = canonical;
                }
            }
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "krusty-verify-layout-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("kotlinc")).expect("reference");
        root
    }

    #[test]
    fn separate_directories_are_accepted() {
        let root = scratch("separate");
        assert_eq!(
            check_layout(
                &root.join("kotlinc"),
                Some(&root.join("krusty/classes")),
                &root.join("records/main.json")
            ),
            Ok(())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn overlapping_paths_are_refused_however_spelled() {
        let root = scratch("overlap");
        let reference = root.join("kotlinc");
        let record = root.join("main.json");
        for destination in [
            reference.clone(),
            reference.join("nested"),
            root.clone(),
            root.join("krusty/../kotlinc"),
        ] {
            assert_eq!(
                check_layout(&reference, Some(&destination), &record),
                Err(format!(
                    "-d {} overlaps --reference {}; kotlinc's output, krusty's output and the record must be separate",
                    destination.display(),
                    reference.display()
                )),
                "{}",
                destination.display()
            );
        }
        assert_eq!(
            check_layout(&reference, Some(&root.join("krusty")), &reference.join("main.json")),
            Err(format!(
                "--record {} overlaps --reference {}; kotlinc's output, krusty's output and the record must be separate",
                reference.join("main.json").display(),
                reference.display()
            ))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_to_the_reference_is_refused() {
        let root = scratch("link");
        let reference = root.join("kotlinc");
        let link = root.join("alias");
        std::os::unix::fs::symlink(&reference, &link).expect("link");
        assert_eq!(
            check_layout(&reference, Some(&link.join("classes")), &root.join("main.json")),
            Err(format!(
                "-d {} overlaps --reference {}; kotlinc's output, krusty's output and the record must be separate",
                link.join("classes").display(),
                reference.display()
            ))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_after_a_missing_directory_and_its_parent_is_refused() {
        let root = scratch("missing-link");
        let reference = root.join("kotlinc");
        std::os::unix::fs::symlink(&reference, root.join("alias")).expect("link");
        let destination = root.join("missing/../alias/classes");
        assert_eq!(
            check_layout(&reference, Some(&destination), &root.join("main.json")),
            Err(format!(
                "-d {} overlaps --reference {}; kotlinc's output, krusty's output and the record must be separate",
                destination.display(),
                reference.display()
            ))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn jars_are_refused() {
        let root = scratch("jar");
        assert_eq!(
            check_layout(
                &root.join("kotlinc"),
                Some(&root.join("out.jar")),
                &root.join("main.json")
            ),
            Err(format!(
                "-d {} names a jar; verify-compile compares class directories",
                root.join("out.jar").display()
            ))
        );
        std::fs::write(root.join("kotlinc.jar"), b"PK").expect("jar");
        assert_eq!(
            check_layout(
                &root.join("kotlinc.jar"),
                Some(&root.join("krusty")),
                &root.join("main.json")
            ),
            Err(format!(
                "--reference {} is a file; verify-compile compares class directories",
                root.join("kotlinc.jar").display()
            ))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_request_without_a_destination_checks_only_the_reference_and_record() {
        let root = scratch("record-only");
        let reference = root.join("kotlinc");
        assert_eq!(
            check_layout(&reference, None, &root.join("main.json")),
            Ok(())
        );
        assert_eq!(
            check_layout(&reference, None, &reference.join("main.json")),
            Err(format!(
                "--record {} overlaps --reference {}; kotlinc's output, krusty's output and the record must be separate",
                reference.join("main.json").display(),
                reference.display()
            ))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_record_named_like_a_report_in_the_output_is_refused() {
        let root = scratch("report");
        let out = root.join("report");
        let record = root.join("records/../report/krusty-verify.md");
        assert_eq!(
            check_report_layout(&out, &[root.join("main.json"), root.join("records/a.json")]),
            Ok(())
        );
        assert_eq!(
            check_report_layout(&out, std::slice::from_ref(&record)),
            Err(format!(
                "record {} is a report verify-report writes to --out {}",
                record.display(),
                out.display()
            ))
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
