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
}
