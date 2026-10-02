//! Workspace files admitted to one background index chunk.
//!
//! URI conversion stays in `uri`. This module owns the read budget: a file whose metadata already
//! exceeds the remaining budget is skipped before its contents are allocated, and the length is
//! checked again after the read so a file that grows between the two is not charged an inaccurate
//! size.

use crate::uri::file_uri_to_path;

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
