//! Invocation-owned records, observed only after the task has drained its child processes.
//!
//! Exclusive creation of an empty owner file assigns a numeric slot before writing. Slots stay
//! claimed until the completed run is consumed, so a later writer cannot acquire an earlier writer's ownership.
//! Numeric claim order preserves dependency/KSP order without depending on completion order.

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

pub struct InvocationRecords {
    directory: PathBuf,
}

impl InvocationRecords {
    pub fn new(run: &Path) -> Self {
        Self {
            directory: run.join("record"),
        }
    }

    /// Own records at an explicit lane path, created only when that lane is used.
    pub fn at(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
        }
    }

    pub fn create(&self) -> io::Result<()> {
        std::fs::create_dir(&self.directory)
    }

    pub fn path(&self) -> &Path {
        &self.directory
    }

    /// A non-consuming snapshot, including earlier completed runs (KSP avoidance uses this).
    /// Missing, incomplete or malformed slots are errors, never an empty successful record.
    pub fn read_snapshot(&self) -> io::Result<Vec<u8>> {
        let mut slots = Vec::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let index = name.to_str().and_then(|name| name.parse::<usize>().ok());
            let Some(index) = index.filter(|index| name == index.to_string().as_str()) else {
                return Err(self.invalid("non-numeric invocation slot"));
            };
            if !entry.file_type()?.is_dir() {
                return Err(self.invalid("invocation slot is not a directory"));
            }
            slots.push(index);
        }
        slots.sort_unstable();
        let mut record = Vec::new();
        for (expected, index) in slots.into_iter().enumerate() {
            if index != expected {
                return Err(self.invalid("missing invocation slot"));
            }
            let slot = self.directory.join(index.to_string());
            let files = std::fs::read_dir(&slot)?.collect::<io::Result<Vec<_>>>()?;
            if files.len() != 2 {
                return Err(self.invalid("incomplete invocation slot"));
            }
            for file in &files {
                if !file.file_type()?.is_file()
                    || (file.file_name() != "arguments"
                        && (file.file_name() != "owner" || file.metadata()?.len() != 0))
                {
                    return Err(self.invalid("incomplete invocation slot"));
                }
            }
            let path = slot.join("arguments");
            let arguments = std::fs::read(&path)?;
            if !arguments.ends_with(b"----\n") {
                return Err(self.invalid(&format!(
                    "unterminated invocation record at {} ({} bytes): {arguments:?}",
                    path.display(),
                    arguments.len()
                )));
            }
            record.extend(arguments);
        }
        Ok(record)
    }

    /// Call only after all children have terminated. Validate before deleting any evidence.
    pub fn consume_after_run(&self) -> io::Result<Vec<u8>> {
        let record = self.read_snapshot()?;
        std::fs::remove_dir_all(&self.directory)?;
        self.create()?;
        Ok(record)
    }

    /// Validate a completed lane before clearing it; an unused lane stays absent.
    pub fn clear_after_run(&self) -> io::Result<()> {
        match std::fs::symlink_metadata(&self.directory) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        self.read_snapshot()?;
        std::fs::remove_dir_all(&self.directory)
    }

    fn invalid(&self, message: &str) -> io::Error {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("{}: {message}", self.directory.display()),
        )
    }
}

/// POSIX writer shared by compiler, Java/KSP and Gradle proxy fixtures.
pub const WRITE_INVOCATION: &str = r#"record_invocation() {
  if [ ! -d "$KRUSTY_RECORD" ]; then
    echo "recorder: missing invocation directory $KRUSTY_RECORD" >&2
    exit 7
  fi
  record_index=0
  while :; do
    record_slot="$KRUSTY_RECORD/$record_index"
    if ! mkdir "$record_slot" 2>/dev/null && [ ! -d "$record_slot" ]; then
      echo "recorder: cannot claim invocation slot $record_slot" >&2
      exit 7
    fi
    # Directory creation is structural: some mkdir implementations also report a
    # racing creator's directory as success. Only exclusive file creation owns it.
    if (set -C; : > "$record_slot/owner") 2>/dev/null; then break; fi
    if [ ! -f "$record_slot/owner" ]; then
      printf '%s\n' "cannot claim invocation slot: $record_slot" >&2
      exit 7
    fi
    record_index=$((record_index + 1))
  done
  if ! {
    for recorded_argument in "$@"; do printf '%s\n' "$recorded_argument" || exit 7; done
    printf '%s\n' '----'
  } > "$record_slot/pending"; then
    echo "recorder: cannot write invocation slot $record_slot" >&2
    exit 7
  fi
  if ! mv "$record_slot/pending" "$record_slot/arguments"; then
    echo "recorder: cannot publish invocation slot $record_slot" >&2
    exit 7
  fi
}
"#;
