use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    "build",
    "node_modules",
    "out",
    "target",
];

/// Upper bound on walkers.
///
/// A tree walk waits on directory reads rather than working the CPU, so more walkers than cores is
/// the point: over one large tree, 4 walkers give 1.5x, 8 give 1.8x, 16 give 2.3x and 32 give 2.7x.
/// Sixteen is where intellij-community stops improving and it leaves the cores for the analysis
/// this runs beside.
const MAX_WALK_THREADS: usize = 16;

/// Directories to discover before recruiting help. A small workspace finishes inside this and never
/// pays for a thread it did not need.
const PARALLEL_WALK_THRESHOLD: usize = 16;

pub(super) fn is_ignored_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    if name == "buildSrc" || name == "build-logic" {
        return false;
    }
    IGNORED_DIRECTORIES.contains(&name)
}

/// One directory to walk, and whether the walk may skip build output beneath it.
///
/// The flag travels with the root because the two callers disagree: strict discovery follows a
/// source root wherever it points, while the workspace inventory refuses to descend into build
/// output.
pub(super) struct WalkRoot {
    pub(super) path: PathBuf,
    pub(super) ignore_workspace_directories: bool,
}

/// Traversal failed after collecting a possibly useful prefix.
///
/// The walker deliberately carries no user-facing diagnostic policy: strict project loading maps
/// this to its semantic-diagnostics suppression message, while workspace inventory keeps the prefix
/// and marks the snapshot truncated. Keeping the error semantic and local prevents the generic
/// traversal layer from depending back on either caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WalkError;

/// Collect every Kotlin and Java source under `roots`.
///
/// Every root is walked by one shared set of walkers, which is the whole point: a source root is
/// usually a few dozen package directories, so a thread pool per root would set itself up and tear
/// itself down for a handful of reads, and measured 2-3x *slower* than not threading at all. Given
/// every root at once there is enough work to be worth dividing, and the walkers balance across
/// roots without anyone deciding how.
///
/// Walking the tree is the dominant cost of a cold start and the one part of indexing that is not
/// incremental. It also waits on directory reads rather than working the CPU, which is the shape
/// threads help: one directory's contents never depend on another's.
///
/// `excluded` are directories to stop at, given sorted. A root nested inside another belongs to
/// this list, so the outer walk leaves it to its own entry rather than walking it twice.
///
/// The traversal is unlimited in depth and entry count -- a large tree must scan completely -- and
/// `progress` receives the discovered count at most every `interval` and once at the end, so a long
/// scan is visible without flooding the client. It is called only from this thread.
///
/// Results arrive in whatever order the walkers finish. Both callers sort, and a tree walk had no
/// meaningful order to begin with.
pub(super) fn walk_sources(
    roots: &[WalkRoot],
    excluded: &[PathBuf],
    sources: &mut Vec<PathBuf>,
    interval: Duration,
    progress: &mut dyn FnMut(crate::ScanProgress),
) -> Result<(), WalkError> {
    walk_sources_with(roots, excluded, sources, interval, walker_count(), progress)
}

/// Twice the cores, bounded: enough oversubscription to keep reads in flight without spawning
/// sixteen threads on a machine with two.
fn walker_count() -> usize {
    std::thread::available_parallelism()
        .map(|threads| threads.get())
        .unwrap_or(1)
        .saturating_mul(2)
        .clamp(2, MAX_WALK_THREADS)
}

fn walk_sources_with(
    roots: &[WalkRoot],
    excluded: &[PathBuf],
    sources: &mut Vec<PathBuf>,
    interval: Duration,
    threads: usize,
    progress: &mut dyn FnMut(crate::ScanProgress),
) -> Result<(), WalkError> {
    if roots.is_empty() {
        progress(crate::ScanProgress::Found { files: 0 });
        return Ok(());
    }
    let queue = WalkQueue::new(roots);
    let found = AtomicUsize::new(0);
    let walk = Walk {
        queue: &queue,
        found: &found,
        excluded,
    };

    // The first directories are walked here, so a workspace that is smaller than the threshold
    // never starts a thread at all.
    let mut collected = walk.run(Some(PARALLEL_WALK_THRESHOLD), interval, Some(progress));
    if !queue.is_finished() {
        std::thread::scope(|scope| {
            let handles = (0..threads)
                .filter_map(|index| {
                    // A constrained process can refuse another OS thread (`EAGAIN`). The old
                    // `scope.spawn` form panicked here; a fallible builder lets whatever workers
                    // were created share the queue, and the all-failed case below finishes it on
                    // the caller thread instead of abandoning live work.
                    std::thread::Builder::new()
                        .name(format!("krusty-source-walk-{index}"))
                        .spawn_scoped(scope, || {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                walk.run(None, interval, None)
                            }))
                            .unwrap_or_else(|_| {
                                // `ActiveClaim` covers the dangerous directory-reading interval,
                                // but keep the worker boundary guarded as well: a future panic
                                // between claims would otherwise leave queued work with no worker
                                // and the monitor could wait forever. Failure is idempotent, so it
                                // is safe when the active-claim guard already reported the panic.
                                queue.fail();
                                Vec::new()
                            })
                        })
                        .ok()
                })
                .collect::<Vec<_>>();
            if handles.is_empty() {
                // The queue is still live and no helper exists to release a claim. Run the same
                // generic worker on this thread; it owns progress callbacks just as the serial
                // prefix did. This also makes a deliberately zero-helper test configuration safe.
                collected.extend(walk.run(None, interval, Some(progress)));
            } else {
                let mut last_report = Instant::now();
                while !queue.wait_for_progress(interval) {
                    if last_report.elapsed() >= interval {
                        progress(crate::ScanProgress::Found {
                            files: found.load(Ordering::Relaxed) as u64,
                        });
                        last_report = Instant::now();
                    }
                }
                for handle in handles {
                    // The claim guard and outer worker boundary convert a panic into the same
                    // partial-walk failure. Keep the join defensive as a final scoped-thread
                    // boundary rather than re-panicking on the caller thread.
                    collected.extend(handle.join().unwrap_or_default());
                }
            }
        });
    }

    let failed = queue.failed();
    sources.append(&mut collected);
    if failed {
        // The caller decides what a partial result is worth: strict discovery discards it, the
        // workspace inventory keeps it and reports itself incomplete. No final count is reported,
        // because a count that stopped early is not the total it would be read as.
        return Err(WalkError);
    }
    progress(crate::ScanProgress::Found {
        files: sources.len() as u64,
    });
    Ok(())
}

struct Walk<'a> {
    queue: &'a WalkQueue,
    found: &'a AtomicUsize,
    excluded: &'a [PathBuf],
}

impl Walk<'_> {
    /// Read directories until the queue is empty, or until `limit` of them have been read.
    ///
    /// Children go straight back to the shared queue rather than onto a local stack. Keeping them
    /// was measurably slower: the walk is bound by directory reads, not by the lock, so a walker
    /// that hoards its children only starves the walkers that could have read them.
    ///
    /// `progress`, when given, is the caller's own thread reporting its serial prefix; walkers
    /// never report, they only publish their running count.
    fn run(
        &self,
        limit: Option<usize>,
        interval: Duration,
        mut progress: Option<&mut dyn FnMut(crate::ScanProgress)>,
    ) -> Vec<PathBuf> {
        let mut sources = Vec::new();
        let mut read = 0usize;
        let mut published = 0usize;
        let mut last_report = Instant::now();
        while limit.is_none_or(|limit| read < limit) {
            let Some(directory) = self.queue.claim() else {
                break;
            };
            // A claim is structural queue state, so pair it with RAII rather than relying on every
            // future directory-reading branch to remember `release`. If code inside a worker ever
            // panics, dropping this guard decrements `reading`, marks the walk failed, and wakes the
            // monitor instead of leaving it blocked forever on work that can no longer complete.
            let claimed = ActiveClaim::new(self.queue, directory);
            read += 1;
            let mut children = Vec::new();
            let failed = self.read_directory(claimed.directory(), &mut children, &mut sources);
            claimed.release(children, failed);
            // Published as the walk runs, not once it returns: the count a walker holds privately
            // is the count nobody can report, and the parallel phase is the whole scan on any
            // workspace big enough to reach it.
            self.found
                .fetch_add(sources.len() - published, Ordering::Relaxed);
            published = sources.len();
            if failed {
                break;
            }
            if let Some(progress) = progress.as_mut() {
                if last_report.elapsed() >= interval {
                    progress(crate::ScanProgress::Found {
                        files: self.found.load(Ordering::Relaxed) as u64,
                    });
                    last_report = Instant::now();
                }
            }
        }
        sources
    }

    /// Returns whether the directory could not be read.
    fn read_directory(
        &self,
        claimed: &ClaimedDirectory,
        children: &mut Vec<ClaimedDirectory>,
        sources: &mut Vec<PathBuf>,
    ) -> bool {
        let entries = match fs::read_dir(&claimed.path) {
            Ok(entries) => entries,
            // A directory that vanished between being queued and being read is not an error; the
            // tree is allowed to change under a scan this long.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            let Ok(kind) = entry.file_type() else {
                return true;
            };
            if kind.is_dir() {
                let path = entry.path();
                if self.excluded.binary_search(&path).is_ok() {
                    continue;
                }
                if claimed.ignore_workspace_directories && is_ignored_directory(&path) {
                    continue;
                }
                children.push(ClaimedDirectory {
                    path,
                    ignore_workspace_directories: claimed.ignore_workspace_directories,
                });
            } else if kind.is_file() && is_source_name(&entry.file_name()) {
                // The path is built only for a file that is kept. Every entry used to allocate one,
                // and a large tree holds several times more entries than sources.
                sources.push(entry.path());
            }
        }
        false
    }
}

/// One queue claim that must be released exactly once.
struct ActiveClaim<'a> {
    queue: &'a WalkQueue,
    directory: Option<ClaimedDirectory>,
}

impl<'a> ActiveClaim<'a> {
    fn new(queue: &'a WalkQueue, directory: ClaimedDirectory) -> Self {
        Self {
            queue,
            directory: Some(directory),
        }
    }

    fn directory(&self) -> &ClaimedDirectory {
        self.directory
            .as_ref()
            .expect("an active queue claim still owns its directory")
    }

    fn release(mut self, directories: Vec<ClaimedDirectory>, failed: bool) {
        self.queue.release(directories, failed);
        self.directory = None;
    }
}

impl Drop for ActiveClaim<'_> {
    fn drop(&mut self) {
        if self.directory.take().is_some() {
            self.queue.release(Vec::new(), true);
        }
    }
}

/// Whether a directory entry names a source.
///
/// Deliberately the same predicate the walker has always applied, expressed against the file name
/// so an entry that is not kept costs no path: `Path::extension` on the name alone, which treats a
/// leading dot as the whole name rather than an extension, so `.kt` is not a Kotlin source.
fn is_source_name(name: &std::ffi::OsStr) -> bool {
    let name = Path::new(name);
    krusty::source::is_supported_path(name)
        || name.extension().and_then(|extension| extension.to_str()) == Some("java")
}

/// A directory to read, carrying the policy of the root it came from.
struct ClaimedDirectory {
    path: PathBuf,
    ignore_workspace_directories: bool,
}

/// Directories waiting to be read, and how many are being read right now.
///
/// The pair is what makes termination decidable: work is exhausted only when nothing is queued
/// *and* nobody is still reading, because a directory being read can still produce children.
struct WalkQueue {
    state: Mutex<WalkState>,
    ready: Condvar,
}

struct WalkState {
    pending: Vec<ClaimedDirectory>,
    reading: usize,
    failed: bool,
}

impl WalkQueue {
    fn new(roots: &[WalkRoot]) -> Self {
        Self {
            state: Mutex::new(WalkState {
                pending: roots
                    .iter()
                    .map(|root| ClaimedDirectory {
                        path: root.path.clone(),
                        ignore_workspace_directories: root.ignore_workspace_directories,
                    })
                    .collect(),
                reading: 0,
                failed: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WalkState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Take a directory and count the caller as reading until it releases.
    fn claim(&self) -> Option<ClaimedDirectory> {
        let mut state = self.lock();
        loop {
            if state.failed {
                return None;
            }
            if let Some(directory) = state.pending.pop() {
                state.reading += 1;
                return Some(directory);
            }
            if state.reading == 0 {
                // Nothing queued and nobody reading: no further work can appear. Wake everyone
                // else waiting on the same conclusion.
                self.ready.notify_all();
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Give up the claim, queueing whatever the directory contained.
    fn release(&self, directories: Vec<ClaimedDirectory>, failed: bool) {
        let mut state = self.lock();
        state.pending.extend(directories);
        debug_assert!(
            state.reading > 0,
            "every claimed directory is released exactly once"
        );
        state.reading = state.reading.saturating_sub(1);
        state.failed |= failed;
        self.ready.notify_all();
    }

    fn is_finished(&self) -> bool {
        let state = self.lock();
        state.is_finished()
    }

    /// Block until the walk finishes or `timeout` elapses, reporting whether it finished.
    fn wait_for_progress(&self, timeout: Duration) -> bool {
        let state = self.lock();
        if state.is_finished() {
            return true;
        }
        let (state, _) = self
            .ready
            .wait_timeout(state, timeout)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.is_finished()
    }

    fn failed(&self) -> bool {
        self.lock().failed
    }

    /// Stop the walk and wake every claimant or monitor.
    ///
    /// This is separate from releasing a directory because a worker can fail between claims. The
    /// operation is intentionally idempotent: an unwinding `ActiveClaim` and the worker boundary
    /// may both observe the same panic, and either one must be sufficient to unblock termination.
    fn fail(&self) {
        self.lock().failed = true;
        self.ready.notify_all();
    }
}

impl WalkState {
    fn is_finished(&self) -> bool {
        self.failed || (self.pending.is_empty() && self.reading == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::testing::TempTree;

    fn root(path: &Path, ignore_workspace_directories: bool) -> WalkRoot {
        WalkRoot {
            path: path.to_path_buf(),
            ignore_workspace_directories,
        }
    }

    fn walk(path: &Path, ignore: bool, excluded: &[PathBuf]) -> (Vec<PathBuf>, bool) {
        let mut sources = Vec::new();
        let failed = walk_sources(
            std::slice::from_ref(&root(path, ignore)),
            excluded,
            &mut sources,
            Duration::from_millis(1),
            &mut |_| {},
        )
        .is_err();
        sources.sort();
        (sources, failed)
    }

    #[test]
    fn a_wide_tree_is_walked_completely() {
        let tree = TempTree::new("walk-wide");
        // Past the threshold that recruits threads, so this covers the handover as well as the
        // result: every file must appear exactly once whichever walker found it.
        let mut expected = Vec::new();
        for directory in 0..64 {
            for file in 0..4 {
                expected.push(tree.write(
                    &format!("module{directory}/src/File{file}.kt"),
                    "class Type\n",
                ));
            }
        }
        expected.sort();

        let (found, failed) = walk(tree.root(), true, &[]);

        assert!(!failed);
        assert_eq!(found, expected);
    }

    #[test]
    fn a_deep_chain_is_walked_completely() {
        let tree = TempTree::new("walk-deep");
        // One directory per level: there is never more than one queued, so every walker but one is
        // waiting on the one that is reading. Termination has to survive that.
        let mut relative = String::new();
        for level in 0..64 {
            relative.push_str(&format!("level{level}/"));
        }
        let expected = tree.write(&format!("{relative}Deep.kt"), "class Deep\n");

        let (found, failed) = walk(tree.root(), true, &[]);

        assert!(!failed);
        assert_eq!(found, vec![expected]);
    }

    #[test]
    fn ignored_directories_are_not_descended() {
        let tree = TempTree::new("walk-ignored");
        let kept = tree.write("src/Kept.kt", "class Kept\n");
        tree.write("build/Generated.kt", "class Generated\n");
        tree.write("target/Stale.kt", "class Stale\n");
        let build_src = tree.write("buildSrc/src/Plugin.kt", "class Plugin\n");

        let (found, failed) = walk(tree.root(), true, &[]);

        assert!(!failed);
        let mut expected = vec![kept, build_src];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn ignored_directories_are_descended_when_the_caller_asks() {
        let tree = TempTree::new("walk-unignored");
        let kept = tree.write("src/Kept.kt", "class Kept\n");
        let generated = tree.write("build/Generated.kt", "class Generated\n");

        let (found, failed) = walk(tree.root(), false, &[]);

        assert!(!failed);
        let mut expected = vec![kept, generated];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn an_excluded_source_root_is_left_to_its_own_walk() {
        let tree = TempTree::new("walk-excluded");
        let kept = tree.write("src/Kept.kt", "class Kept\n");
        tree.write("nested/Nested.kt", "class Nested\n");
        let excluded = vec![tree.path("nested")];

        let (found, failed) = walk(tree.root(), true, &excluded);

        assert!(!failed);
        assert_eq!(found, vec![kept]);
    }

    #[test]
    fn every_root_is_walked_by_the_same_walkers() {
        let tree = TempTree::new("walk-many-roots");
        // The shape production actually has: many small roots rather than one large tree. Each is
        // far too small to be worth its own set of walkers, which is why they share one.
        let mut expected = Vec::new();
        let mut roots = Vec::new();
        for module in 0..48 {
            let source_root = tree.directory(&format!("module{module}/src/main/kotlin"));
            for file in 0..3 {
                expected.push(tree.write(
                    &format!("module{module}/src/main/kotlin/File{file}.kt"),
                    "class Type\n",
                ));
            }
            roots.push(root(&source_root, true));
        }
        expected.sort();

        let mut sources = Vec::new();
        walk_sources(
            &roots,
            &[],
            &mut sources,
            Duration::from_millis(1),
            &mut |_| {},
        )
        .unwrap();
        sources.sort();

        assert_eq!(sources, expected);
    }

    #[test]
    fn a_root_nested_in_another_is_walked_once() {
        let tree = TempTree::new("walk-nested-roots");
        let outer = tree.directory("outer");
        let inner = tree.directory("outer/inner");
        let outer_file = tree.write("outer/Outer.kt", "class Outer\n");
        let inner_file = tree.write("outer/inner/Inner.kt", "class Inner\n");
        let roots = vec![root(&outer, true), root(&inner, true)];
        let mut excluded = vec![outer.clone(), inner.clone()];
        excluded.sort();

        let mut sources = Vec::new();
        walk_sources(
            &roots,
            &excluded,
            &mut sources,
            Duration::from_millis(1),
            &mut |_| {},
        )
        .unwrap();
        sources.sort();

        let mut expected = vec![outer_file, inner_file];
        expected.sort();
        assert_eq!(
            sources, expected,
            "the outer walk stops at the nested root, which walks itself"
        );
    }

    #[test]
    fn a_single_walker_covers_the_tree() {
        let tree = TempTree::new("walk-single-walker");
        let mut expected = Vec::new();
        for directory in 0..48 {
            expected.push(tree.write(&format!("d{directory}/File.kt"), "class Type\n"));
        }
        expected.sort();

        // A machine reporting one core still gets two walkers, but the walk has to be correct with
        // any number: one walker means every wait is on work it queued itself.
        let mut sources = Vec::new();
        walk_sources_with(
            std::slice::from_ref(&root(tree.root(), true)),
            &[],
            &mut sources,
            Duration::from_millis(1),
            1,
            &mut |_| {},
        )
        .unwrap();
        sources.sort();

        assert_eq!(sources, expected);
    }

    #[test]
    fn no_spawned_helper_falls_back_to_the_caller_thread() {
        let tree = TempTree::new("walk-no-helper");
        let mut expected = Vec::new();
        for directory in 0..48 {
            expected.push(tree.write(&format!("d{directory}/File.kt"), "class Type\n"));
        }
        expected.sort();

        // `threads == 0` deterministically exercises the same branch used when every fallible
        // `spawn_scoped` call is refused by the OS. The serial prefix stops at the parallel
        // threshold, so correctness here proves the remaining live queue is drained rather than
        // left waiting forever for a helper that does not exist.
        let mut sources = Vec::new();
        walk_sources_with(
            std::slice::from_ref(&root(tree.root(), true)),
            &[],
            &mut sources,
            Duration::from_millis(1),
            0,
            &mut |_| {},
        )
        .unwrap();
        sources.sort();

        assert_eq!(sources, expected);
    }

    #[test]
    fn a_panicking_claim_is_released_as_a_failed_walk() {
        let tree = TempTree::new("walk-panicking-claim");
        let roots = [root(tree.root(), true)];
        let queue = WalkQueue::new(&roots);
        let directory = queue.claim().expect("the synthetic root is queued");

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _claim = ActiveClaim::new(&queue, directory);
            panic!("synthetic worker failure");
        }));

        assert!(panic.is_err());
        assert!(
            queue.failed(),
            "an abandoned claim makes the partial walk fail"
        );
        assert!(
            queue.is_finished(),
            "the abandoned claim must not leave `reading` above zero"
        );
        assert!(queue.claim().is_none());
    }

    #[test]
    fn a_read_error_returns_the_prefix_without_a_final_total() {
        let tree = TempTree::new("walk-partial-error");
        let valid_root = tree.directory("valid");
        let kept = tree.write("valid/Kept.kt", "class Kept\n");
        let invalid_root = tree.write("not-a-directory", "");
        // The queue pops from the end: visit the valid root first, then fail when `read_dir` sees
        // the regular file. This pins the generic walker contract that callers interpret
        // differently: the prefix survives, but success-only final progress is not published.
        let roots = vec![root(&invalid_root, true), root(&valid_root, true)];
        let mut sources = Vec::new();
        let mut reports = Vec::new();

        let result = walk_sources_with(
            &roots,
            &[],
            &mut sources,
            Duration::from_secs(60),
            1,
            &mut |event| {
                if let crate::ScanProgress::Found { files } = event {
                    reports.push(files);
                }
            },
        );

        assert_eq!(result, Err(WalkError));
        assert_eq!(sources, vec![kept]);
        assert!(
            reports.is_empty(),
            "a partial count must not be reported as a completed total"
        );
    }

    #[test]
    fn a_dotfile_named_like_an_extension_is_not_a_source() {
        let tree = TempTree::new("walk-dotfiles");
        let kept = tree.write("Real.kt", "class Real\n");
        // `.kt` is a name, not an extension. Matching on the text after the last dot would collect
        // it, and strict discovery would then hand it to the Java source path.
        tree.write(".kt", "");
        tree.write(".kts", "");
        tree.write(".java", "");
        tree.write("NoExtension", "");
        tree.write("Upper.KT", "");

        let (found, failed) = walk(tree.root(), true, &[]);

        assert!(!failed);
        assert_eq!(found, vec![kept]);
    }

    #[test]
    fn progress_climbs_while_the_walkers_run() {
        let tree = TempTree::new("walk-progress-live");
        // Wide enough to need the walkers, so the reports come from the parallel phase rather than
        // from the serial prefix.
        for directory in 0..256 {
            for file in 0..4 {
                tree.write(
                    &format!("module{directory}/src/File{file}.kt"),
                    "class Type\n",
                );
            }
        }
        let mut reports = Vec::new();

        let mut sources = Vec::new();
        walk_sources(
            std::slice::from_ref(&root(tree.root(), true)),
            &[],
            &mut sources,
            Duration::ZERO,
            &mut |event| {
                if let crate::ScanProgress::Found { files } = event {
                    reports.push(files);
                }
            },
        )
        .unwrap();

        assert_eq!(sources.len(), 1024);
        assert_eq!(reports.last().copied(), Some(1024));
        // A count published only when a walker finishes is a count nobody can report: every
        // intermediate report would repeat whatever the serial prefix happened to see.
        let distinct = reports.iter().collect::<std::collections::HashSet<_>>();
        assert!(
            distinct.len() > 1,
            "progress must move while the walk runs, got {reports:?}"
        );
    }

    #[test]
    fn a_missing_root_is_not_a_failure() {
        let tree = TempTree::new("walk-missing");

        let (found, failed) = walk(&tree.path("absent"), true, &[]);

        assert!(!failed, "a directory that is not there is not a read error");
        assert!(found.is_empty());
    }

    #[test]
    fn progress_is_reported_and_ends_with_the_total() {
        let tree = TempTree::new("walk-progress");
        for directory in 0..32 {
            tree.write(&format!("module{directory}/File.kt"), "class Type\n");
        }
        let mut reports = Vec::new();

        let mut sources = Vec::new();
        walk_sources(
            std::slice::from_ref(&root(tree.root(), true)),
            &[],
            &mut sources,
            Duration::from_millis(1),
            &mut |event| {
                if let crate::ScanProgress::Found { files } = event {
                    reports.push(files);
                }
            },
        )
        .unwrap();

        assert_eq!(sources.len(), 32);
        assert_eq!(
            reports.last().copied(),
            Some(32),
            "the last report is the total, whatever came before it"
        );
    }
}
