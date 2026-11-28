//! The persistent box runner runs a class file on a JDK that can load it.
//!
//! A file that pins `-jvm-target 25` fails with `UnsupportedClassVersionError` on a JDK 21 host
//! unless the runner for that request runs on a provisioned JDK 25. The end-to-end behavior is
//! covered by every e2e file that pins a newer target; these tests pin the selection itself.

use super::common::jdk_homes::release_of_record;
use super::common::runtime_jdk::{choose, feature_release_for_class_major, newest_class_major};
use std::path::PathBuf;

fn class_with_major(major: u16) -> (String, Vec<u8>) {
    let mut bytes = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0];
    bytes.extend_from_slice(&major.to_be_bytes());
    ("A".to_string(), bytes)
}

fn homes() -> Vec<(u32, PathBuf)> {
    [11, 17, 21, 25]
        .into_iter()
        .map(|release| (release, PathBuf::from(format!("/jdks/{release}"))))
        .collect()
}

#[test]
fn a_class_major_names_the_feature_release_that_loads_it() {
    assert_eq!(
        [52, 55, 61, 65, 69].map(feature_release_for_class_major),
        [8, 11, 17, 21, 25]
    );
}

#[test]
fn the_newest_class_major_ignores_files_without_a_class_header() {
    let classes = vec![
        class_with_major(61),
        ("notes.txt".to_string(), b"not a class file".to_vec()),
        class_with_major(69),
        class_with_major(52),
    ];
    assert_eq!(newest_class_major(&classes), Some(69));
    assert_eq!(newest_class_major(&[]), None);
}

#[test]
fn a_release_record_names_its_feature_release() {
    assert_eq!(
        [
            "JAVA_VERSION=\"25.0.1\"\nIMPLEMENTOR=\"Eclipse Adoptium\"\n",
            "JAVA_VERSION=\"21\"\n",
            "JAVA_VERSION=\"1.8.0_402\"\n",
            "JAVA_VERSION=\"17.0.9-ea\"\n",
        ]
        .map(release_of_record),
        [Some(25), Some(21), Some(8), Some(17)]
    );
    assert_eq!(release_of_record("IMPLEMENTOR=\"x\"\n"), None);
}

#[test]
fn the_selected_jdk_runs_a_class_it_can_load() {
    assert_eq!(choose(Some(21), 21, &homes()), None);
    assert_eq!(choose(Some(21), 17, &homes()), None);
    assert_eq!(
        choose(None, 25, &homes()),
        None,
        "a home with no release record keeps the selected JDK"
    );
}

#[test]
fn a_newer_class_runs_on_the_oldest_provisioned_jdk_that_loads_it() {
    assert_eq!(
        choose(Some(21), 25, &homes()),
        Some(PathBuf::from("/jdks/25"))
    );
    assert_eq!(
        choose(Some(17), 21, &homes()),
        Some(PathBuf::from("/jdks/21"))
    );
}

#[test]
fn no_provisioned_jdk_that_loads_the_class_keeps_the_selected_one() {
    assert_eq!(choose(Some(21), 26, &homes()), None);
    assert_eq!(choose(Some(21), 25, &[]), None);
}
