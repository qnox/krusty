use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static PROJECT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct TempProject {
    root: PathBuf,
}

#[allow(dead_code)] // Each integration-test binary uses a different subset of the shared fixture.
impl TempProject {
    pub fn new(label: &str) -> Self {
        let unique = PROJECT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "krusty-lsp-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temporary project");
        Self { root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create source directory");
        }
        std::fs::write(&path, source).expect("write project source");
        path
    }

    pub fn uri(&self, relative: &str) -> String {
        url::Url::from_file_path(self.root.join(relative))
            .expect("temporary project path is a file URI")
            .into()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}
