//! Workspace files read for one background index chunk.
//!
//! URI conversion stays in `uri`. A chunk is bounded by its file count; every file in it is read
//! whatever its size, because a file left out of a chunk is treated as deleted by the store that
//! receives the result.

use crate::uri::file_uri_to_path;

/// Read workspace sources for one index chunk. An unreadable or deleted file is omitted.
pub fn read_index_chunk(uris: &[&str]) -> Vec<(String, String)> {
    uris.iter()
        .filter_map(|uri| {
            let path = file_uri_to_path(uri)?;
            let text = std::fs::read_to_string(path).ok()?;
            Some(((*uri).to_string(), text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri::path_to_file_uri;

    /// Every readable file of a chunk is read, in order, whatever its size; a missing one is
    /// omitted rather than failing the chunk.
    #[test]
    fn index_chunk_reads_every_readable_file() {
        let directory =
            std::env::temp_dir().join(format!("krusty-index-chunk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let small = directory.join("Small.kt");
        let large = directory.join("Large.kt");
        let large_text = format!("fun large() = 1\n{}", "// padding\n".repeat(1024 * 1024));
        std::fs::write(&small, "fun small() = 1\n").unwrap();
        std::fs::write(&large, &large_text).unwrap();
        let uris = [
            path_to_file_uri(&small).unwrap(),
            path_to_file_uri(&directory.join("Missing.kt")).unwrap(),
            path_to_file_uri(&large).unwrap(),
        ];

        let read = read_index_chunk(&[&uris[0], &uris[1], &uris[2]]);

        assert_eq!(
            read,
            [
                (uris[0].clone(), "fun small() = 1\n".to_string()),
                (uris[2].clone(), large_text),
            ]
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
