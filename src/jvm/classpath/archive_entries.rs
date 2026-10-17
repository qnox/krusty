//! The entry inventory that classpath index builders read from an archive: each entry's decoded
//! name and bytes.
//!
//! Builders read through [`ArchiveEntries`] rather than a concrete `ZipArchive`, so the name-decode
//! failure zip reports per entry reaches them as a value and tests can supply it. zip 9.0.3 decodes
//! every name it opens (UTF-8, else total CP437), so no real archive produces that failure today.

use std::borrow::Cow;
use std::io::{Read, Seek};

use zip::result::{ZipError, ZipResult};

pub(super) trait ArchiveEntries {
    fn entry_count(&self) -> usize;
    /// The decoded name of entry `index`.
    fn entry_name(&self, index: usize) -> ZipResult<Cow<'_, str>>;
    /// The bytes of entry `index`, or `None` when the entry cannot be opened or read.
    fn entry_bytes(&mut self, index: usize) -> Option<Vec<u8>>;
}

impl<R: Read + Seek> ArchiveEntries for zip::ZipArchive<R> {
    fn entry_count(&self) -> usize {
        self.len()
    }

    fn entry_name(&self, index: usize) -> ZipResult<Cow<'_, str>> {
        self.name_for_index(index)
            .unwrap_or(Err(ZipError::FileNotFound))
    }

    fn entry_bytes(&mut self, index: usize) -> Option<Vec<u8>> {
        let mut entry = self.by_index(index).ok()?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }
}

#[cfg(test)]
pub(super) mod test_archives {
    use super::*;
    use std::io::{Cursor, Write};

    /// An in-memory archive holding `entries` in order.
    pub(in crate::jvm::classpath) fn archive(
        entries: &[(&str, &[u8])],
    ) -> zip::ZipArchive<Cursor<Vec<u8>>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(bytes).expect("write entry");
        }
        let bytes = writer.finish().expect("finish archive").into_inner();
        zip::ZipArchive::new(Cursor::new(bytes)).expect("reopen archive")
    }

    /// `inner` whose entry `index` has a name that does not decode.
    pub(in crate::jvm::classpath) struct UndecodableName<A> {
        pub(in crate::jvm::classpath) inner: A,
        pub(in crate::jvm::classpath) index: usize,
    }

    impl<A: ArchiveEntries> ArchiveEntries for UndecodableName<A> {
        fn entry_count(&self) -> usize {
            self.inner.entry_count()
        }

        fn entry_name(&self, index: usize) -> ZipResult<Cow<'_, str>> {
            if index == self.index {
                return Err(ZipError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "undecodable entry name",
                )));
            }
            self.inner.entry_name(index)
        }

        fn entry_bytes(&mut self, index: usize) -> Option<Vec<u8>> {
            self.inner.entry_bytes(index)
        }
    }
}
