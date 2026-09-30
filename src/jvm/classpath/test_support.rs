//! Reusable filesystem fixtures for classpath unit tests.

use std::path::{Path, PathBuf};

pub(super) fn test_temp_dir(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("krusty-{tag}-{}-{unique}", std::process::id()));
    std::fs::create_dir(&directory).expect("create test directory");
    directory
}

pub(super) fn write_test_jar_with_entry(path: &Path, entry_name: &str, bytes: &[u8]) {
    let file = std::fs::File::create(path).expect("create jar");
    let mut writer = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file(entry_name, options).expect("start entry");
    std::io::Write::write_all(&mut writer, bytes).expect("write entry");
    writer.finish().expect("finish jar");
}
