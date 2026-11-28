//! Choosing the JVM that executes the classes krusty emitted.
//!
//! The persistent box runner normally runs on the JDK [`super::java_home`] selects. A class file
//! whose major version is newer than that JDK understands cannot load there (a file that pins
//! `-jvm-target 25` on a JDK 21 host fails with `UnsupportedClassVersionError`), so the runner for
//! such a request runs on the oldest provisioned JDK that can load it. The reference toolchain and
//! the runner driver's javac keep the selected home, so no reference fingerprint depends on this.

use super::jdk_homes::{available_jdks, jdk_release};
use std::path::{Path, PathBuf};

/// The JDK feature release a class file of `major` needs: major 52 is Java 8.
pub fn feature_release_for_class_major(major: u16) -> u32 {
    u32::from(major).saturating_sub(44)
}

/// The newest class-file major version among `classes`, or `None` when none has a class-file header.
pub fn newest_class_major(classes: &[(String, Vec<u8>)]) -> Option<u16> {
    classes
        .iter()
        .filter(|(_, bytes)| bytes.len() >= 8 && bytes[..4] == [0xCA, 0xFE, 0xBA, 0xBE])
        .map(|(_, bytes)| u16::from_be_bytes([bytes[6], bytes[7]]))
        .max()
}

/// The home to run a request on: `selected` when it can load `needed` (or is not identifiable, which
/// keeps the behavior of a host without a `release` record), else the oldest of `provisioned`
/// (feature release, home) that can, else `selected` again so the failure names the real cause.
pub fn choose(
    selected: Option<u32>,
    needed: u32,
    provisioned: &[(u32, PathBuf)],
) -> Option<PathBuf> {
    if selected.is_none_or(|release| release >= needed) {
        return None;
    }
    provisioned
        .iter()
        .filter(|(release, _)| *release >= needed)
        .min_by_key(|(release, _)| *release)
        .map(|(_, home)| home.clone())
}

/// The JDK home to run `classes` on, given the `selected` home that ordinary requests use.
pub fn home_for_classes(selected: &str, classes: &[(String, Vec<u8>)]) -> String {
    let Some(major) = newest_class_major(classes) else {
        return selected.to_string();
    };
    choose(
        jdk_release(Path::new(selected)),
        feature_release_for_class_major(major),
        &available_jdks(&(8..=40).collect::<Vec<_>>())
            .into_iter()
            .collect::<Vec<_>>(),
    )
    .map_or_else(
        || selected.to_string(),
        |home| home.to_string_lossy().into_owned(),
    )
}
