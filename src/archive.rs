//! Opening the zip archives the compiler reads: classpath jars, KLIBs, compiler plugins.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use zip::read::{ArchiveOffset, Config};
use zip::result::ZipResult;
use zip::ZipArchive;

const END_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
const END_RECORD_LEN: usize = 22;

/// Open the zip archive in `file`, selecting the same archive the zip crate's offset detection
/// selects. Detection scans the whole file for central directory signatures: on a 130 MB classpath
/// that scan was 60% of compiling a four-class module. Almost every archive starts at offset 0,
/// where the end of central directory record locates the central directory directly, so the
/// archive is first read at offset 0 and kept only when it is the one detection would select.
/// Anything else, such as a self-executing jar with prepended data or a file holding several
/// archives, is read by detection, as the JVM's own zip reader accepts it.
pub fn open_file(mut file: File) -> ZipResult<ZipArchive<File>> {
    let last = last_end_record(&mut file)?;
    let mut detect = file.try_clone()?;
    if let Some(last) = last {
        let known = Config {
            archive_offset: ArchiveOffset::Known(0),
        };
        if let Ok(archive) = ZipArchive::with_config(known, file) {
            if last.selects(&archive) {
                return Ok(archive);
            }
        }
    }
    detect.rewind()?;
    ZipArchive::new(detect)
}

/// The end of central directory record detection examines first: the last signature in the file.
struct EndRecord {
    files: u16,
    directory_offset: u32,
    comment: Vec<u8>,
}

impl EndRecord {
    /// Whether `archive`, read at offset 0, is read from this record. Detection reads the same
    /// record first and finds its central directory where offset 0 put it, so it reads the same
    /// archive. A record from earlier in the file, read after this one was rejected at offset 0,
    /// has another directory, file count, or comment.
    fn selects(&self, archive: &ZipArchive<File>) -> bool {
        archive.offset() == 0
            && archive.central_directory_start() == u64::from(self.directory_offset)
            && archive.len() == usize::from(self.files)
            && archive.comment() == self.comment.as_slice()
    }
}

/// The record at the last end of central directory signature, when it is a complete zip32 record.
/// A record that may defer to a zip64 one, or a signature in a truncated record, leaves the
/// selection to detection. An archive without a comment ends in its record, so the last 22 bytes
/// are read first and the comment-sized tail only when they hold no signature.
fn last_end_record(file: &mut File) -> io::Result<Option<EndRecord>> {
    let len = file.seek(SeekFrom::End(0))?;
    let mut found = None;
    for window in [END_RECORD_LEN, END_RECORD_LEN + usize::from(u16::MAX)] {
        let tail_len = len.min(window as u64);
        file.seek(SeekFrom::Start(len - tail_len))?;
        let mut tail = vec![0; tail_len as usize];
        file.read_exact(&mut tail)?;
        if let Some(start) = tail
            .windows(END_SIGNATURE.len())
            .rposition(|w| w == END_SIGNATURE)
        {
            found = Some(tail.split_off(start));
            break;
        }
    }
    file.rewind()?;
    Ok(found.as_deref().and_then(EndRecord::parse))
}

impl EndRecord {
    /// The record starting at `record`, the rest of the file after its signature.
    fn parse(record: &[u8]) -> Option<EndRecord> {
        if record.len() < END_RECORD_LEN {
            return None;
        }
        let u16_at = |at: usize| u16::from_le_bytes([record[at], record[at + 1]]);
        let u32_at = |at: usize| {
            u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]])
        };
        let (files, total_files) = (u16_at(8), u16_at(10));
        let (directory_size, directory_offset) = (u32_at(12), u32_at(16));
        let comment_end = END_RECORD_LEN + usize::from(u16_at(20));
        if total_files == u16::MAX
            || directory_size == u32::MAX
            || directory_offset == u32::MAX
            || record.len() < comment_end
        {
            return None;
        }
        Some(EndRecord {
            files,
            directory_offset,
            comment: record[END_RECORD_LEN..comment_end].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use zip::read::{ArchiveOffset, Config};
    use zip::ZipArchive;

    fn archive_bytes(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file(name, options).expect("start entry");
        writer.write_all(contents).expect("write entry");
        writer.finish().expect("finish archive").into_inner()
    }

    fn write_file(bytes: &[u8], tag: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("krusty-archive-{tag}-{}.jar", std::process::id()));
        std::fs::write(&path, bytes).expect("write archive");
        path
    }

    fn open(path: &std::path::Path) -> std::fs::File {
        std::fs::File::open(path).expect("open archive")
    }

    /// Every entry of `archive`, by name, with its contents.
    fn entries(mut archive: ZipArchive<std::fs::File>) -> Vec<(String, String)> {
        (0..archive.len())
            .map(|index| {
                let mut entry = archive.by_index(index).expect("entry");
                let mut text = String::new();
                entry.read_to_string(&mut text).expect("read entry");
                (entry.name().expect("entry name").into_owned(), text)
            })
            .collect()
    }

    /// The entries `open_file` reads from `bytes`, after checking they are the ones detection reads.
    fn read_entries(bytes: &[u8], tag: &str) -> Vec<(String, String)> {
        let path = write_file(bytes, tag);
        let read = entries(super::open_file(open(&path)).expect("read archive"));
        let detected = entries(ZipArchive::new(open(&path)).expect("detect archive"));
        std::fs::remove_file(&path).expect("remove archive");
        assert_eq!(read, detected);
        read
    }

    fn class_entry() -> Vec<(String, String)> {
        vec![("a/A.class".to_owned(), "class bytes".to_owned())]
    }

    #[test]
    fn an_archive_at_offset_zero_opens() {
        assert_eq!(
            read_entries(&archive_bytes("a/A.class", b"class bytes"), "plain"),
            class_entry()
        );
    }

    #[test]
    fn an_archive_after_prepended_data_still_opens() {
        let mut bytes = b"#!/bin/sh\nexec java -jar \"$0\"\n".to_vec();
        bytes.extend(archive_bytes("a/A.class", b"class bytes"));
        assert_eq!(read_entries(&bytes, "prepended"), class_entry());
    }

    #[test]
    fn an_archive_with_a_comment_opens() {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("a/A.class", options)
            .expect("start entry");
        writer.write_all(b"class bytes").expect("write entry");
        writer.set_comment("built by a tool").expect("set comment");
        let bytes = writer.finish().expect("finish archive").into_inner();
        assert_eq!(read_entries(&bytes, "comment"), class_entry());
    }

    #[test]
    fn concatenated_archives_open_the_one_detection_selects() {
        let mut bytes = archive_bytes("a", b"a");
        bytes.extend(archive_bytes("b", b"bb"));
        // At offset 0 the last record's directory is missed and the first archive is read.
        let path = write_file(&bytes, "concatenated-known");
        let known = Config {
            archive_offset: ArchiveOffset::Known(0),
        };
        let at_zero = ZipArchive::with_config(known, open(&path)).expect("read at offset 0");
        std::fs::remove_file(&path).expect("remove archive");
        assert_eq!(entries(at_zero), vec![("a".to_owned(), "a".to_owned())]);

        assert_eq!(
            read_entries(&bytes, "concatenated"),
            vec![("b".to_owned(), "bb".to_owned())]
        );
    }
}
