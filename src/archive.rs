//! Opening the zip archives the compiler reads: classpath jars, KLIBs, compiler plugins.

use std::fs::File;
use std::io::Seek;

use zip::read::{ArchiveOffset, Config};
use zip::result::ZipResult;
use zip::ZipArchive;

/// Open the zip archive in `file`. Almost every archive starts at offset 0, where the end of
/// central directory record locates the central directory directly. Detecting the offset instead
/// scans the whole file for central directory signatures: on a 130 MB classpath that scan was 60%
/// of compiling a four-class module. An archive with prepended data (a self-executing jar) is still
/// read, by detection, as the JVM's own zip reader accepts it.
pub fn open_file(file: File) -> ZipResult<ZipArchive<File>> {
    let mut detect = file.try_clone()?;
    let known = Config {
        archive_offset: ArchiveOffset::Known(0),
    };
    ZipArchive::with_config(known, file).or_else(|_| {
        detect.rewind()?;
        ZipArchive::new(detect)
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    fn archive_bytes() -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("a/A.class", options)
            .expect("start entry");
        writer.write_all(b"class bytes").expect("write entry");
        writer.finish().expect("finish archive").into_inner()
    }

    fn read_entry(prefix: &[u8], tag: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("krusty-archive-{tag}-{}.jar", std::process::id()));
        let mut bytes = prefix.to_vec();
        bytes.extend(archive_bytes());
        std::fs::write(&path, bytes).expect("write archive");
        let mut archive = super::open_file(std::fs::File::open(&path).expect("open archive"))
            .expect("read archive");
        let mut text = String::new();
        archive
            .by_name("a/A.class")
            .expect("entry")
            .read_to_string(&mut text)
            .expect("read entry");
        std::fs::remove_file(&path).expect("remove archive");
        text
    }

    #[test]
    fn an_archive_at_offset_zero_opens() {
        assert_eq!(read_entry(b"", "plain"), "class bytes");
    }

    #[test]
    fn an_archive_after_prepended_data_still_opens() {
        assert_eq!(
            read_entry(b"#!/bin/sh\nexec java -jar \"$0\"\n", "prepended"),
            "class bytes"
        );
    }
}
