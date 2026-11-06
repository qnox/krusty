//! Workspace files admitted to one background index chunk, and the ceilings on the inventory of
//! files awaiting one.
//!
//! URI conversion stays in `uri`. This module owns the read budget: a file whose metadata already
//! exceeds the remaining budget is skipped before its contents are allocated, and the length is
//! checked again after the read so a file that grows between the two is not charged an inaccurate
//! size.

use crate::uri::file_uri_to_path;

/// Global ceiling on one model generation's cached source inventory.
pub const MAX_WORKSPACE_INDEX_FILES: usize = 200_000;
/// Budgeted bytes for one URI record (payload plus `String` header). Monorepos such as
/// intellij-community average ~150-byte file URIs; 256 leaves room for deeper trees so the file
/// count, not the byte budget, is the ceiling a real workspace reaches first.
const WORKSPACE_INDEX_URI_RECORD_BUDGET: usize = 256;
/// URI payload and record bytes retained by the producer's cached source inventory. Derived from
/// the file ceiling: a fixed byte cap smaller than `files x average URI` silently truncated large
/// workspaces long before the advertised file limit.
pub const MAX_WORKSPACE_INDEX_URI_BYTES: usize =
    MAX_WORKSPACE_INDEX_FILES * WORKSPACE_INDEX_URI_RECORD_BUDGET;

/// One owned URI's payload plus its `String` record. Queue layers multiply this by the number of
/// representations they retain, keeping producer and consumer accounting on the same unit.
pub fn workspace_index_uri_bytes(uri: &str) -> usize {
    uri.len().saturating_add(std::mem::size_of::<String>())
}

/// Bytes one index chunk may read. A count of files is not a memory bound.
pub const MAX_INDEX_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Read workspace sources for one index chunk.
pub fn read_index_chunk(uris: &[&str]) -> Vec<(String, String)> {
    read_sources_within_budget(uris, MAX_INDEX_CHUNK_BYTES)
}

fn read_sources_within_budget(uris: &[&str], mut budget: usize) -> Vec<(String, String)> {
    uris.iter()
        .filter_map(|uri| {
            let path = file_uri_to_path(uri)?;
            let metadata_bytes = usize::try_from(std::fs::metadata(&path).ok()?.len()).ok()?;
            if metadata_bytes > budget {
                return None;
            }
            let text = std::fs::read_to_string(path).ok()?;
            budget = budget.checked_sub(text.len())?;
            Some(((*uri).to_string(), text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri::path_to_file_uri;

    #[test]
    fn index_chunk_read_skips_a_file_that_does_not_fit_the_remaining_budget() {
        let directory =
            std::env::temp_dir().join(format!("krusty-index-chunk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let small = directory.join("Small.kt");
        let large = directory.join("Large.kt");
        std::fs::write(&small, "fun small() = 1\n").unwrap();
        std::fs::write(&large, "fun large() = 1\n").unwrap();
        let uris = [
            path_to_file_uri(&small).unwrap(),
            path_to_file_uri(&large).unwrap(),
        ];
        let budget = std::fs::read_to_string(&small).unwrap().len();

        let read = read_sources_within_budget(&[&uris[0], &uris[1]], budget);

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0, uris[0]);
        assert!(read[0].1.contains("small"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
