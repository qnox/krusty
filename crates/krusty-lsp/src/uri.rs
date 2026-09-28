use std::path::{Path, PathBuf};

use url::Url;

pub fn file_uri_to_path(value: &str) -> Option<PathBuf> {
    if value == "file://" {
        return None;
    }
    let uri = Url::parse(value).ok()?;
    // `Url::to_file_path` accepts some hierarchical non-file schemes on Unix. Callers use this
    // helper specifically as the file/non-file boundary, so accepting `editor:/virtual/A.kt`
    // would mislabel a virtual document as a local path and could expose its query data through
    // path-oriented presentation or cache policy.
    (uri.scheme() == "file")
        .then(|| uri.to_file_path().ok())
        .flatten()
}

pub(crate) fn file_uri_or_path(value: &str) -> Option<PathBuf> {
    if value.len() >= 3
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes()[2], b'/' | b'\\')
    {
        return Some(PathBuf::from(value));
    }
    match Url::parse(value) {
        Ok(url) if url.scheme() == "file" => url.to_file_path().ok(),
        Ok(_) => None,
        Err(_) => (!value.is_empty()).then(|| PathBuf::from(value)),
    }
}

pub fn path_to_file_uri(path: &Path) -> Option<String> {
    Url::from_file_path(path).ok().map(Url::into)
}

/// Bytes one index chunk may read. A count of files is not a memory bound.
pub const MAX_INDEX_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Read workspace sources for one index chunk.
///
/// A file whose metadata already exceeds the remaining budget is skipped before its contents are
/// allocated. The length is checked again after the read, so a file that grows between the two
/// does not get charged an inaccurate size.
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

    #[test]
    fn file_uris_round_trip_reserved_characters() {
        let path = Path::new("/workspace/project with #hash");
        let uri = path_to_file_uri(path).unwrap();

        assert_eq!(uri, "file:///workspace/project%20with%20%23hash");
        assert_eq!(file_uri_to_path(&uri), Some(path.to_path_buf()));
    }

    #[test]
    fn non_file_uris_are_not_local_paths() {
        assert_eq!(file_uri_to_path("untitled:Untitled-1"), None);
        assert_eq!(file_uri_to_path("editor:/virtual/A.kt?token=opaque"), None);
        assert_eq!(file_uri_to_path("file://"), None);
        assert_eq!(file_uri_or_path("https://example.com/A.kt"), None);
        assert_eq!(
            file_uri_or_path("/workspace/A.kt"),
            Some(PathBuf::from("/workspace/A.kt"))
        );
        assert_eq!(
            file_uri_or_path(r"C:\workspace\A.kt"),
            Some(PathBuf::from(r"C:\workspace\A.kt"))
        );
    }

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
