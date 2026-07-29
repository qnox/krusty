//! Memory and latency profiling driver for the LSP analysis worker.

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use krusty::jvm::classpath::platform_jdk_modules;
use krusty_lsp::run_analysis_worker;

fn rss_kb() -> u64 {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn collect_kt(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_kt(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "kt") {
            out.push(path);
        }
    }
}

fn collect_jars(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_jars(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jar")
            && !path.to_string_lossy().contains("-sources")
        {
            out.push(path);
        }
    }
}

struct StagingSink {
    frames: usize,
    stage: usize,
    bytes: usize,
    started: Instant,
}

impl Write for StagingSink {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes += buffer.len();
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.frames += 1;
        if self.frames == 1 {
            eprintln!(
                "[{:8.2}s] ready: classpath prepared, rss={} MiB",
                self.started.elapsed().as_secs_f64(),
                rss_kb() / 1024
            );
        } else if self.stage > 0 && (self.frames - 1).is_multiple_of(self.stage) {
            eprintln!(
                "[{:8.2}s] responses={} rss={} MiB wire={} KiB",
                self.started.elapsed().as_secs_f64(),
                self.frames - 1,
                rss_kb() / 1024,
                self.bytes / 1024
            );
        }
        Ok(())
    }
}

fn frame(body: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    out.extend_from_slice(body);
}

#[derive(serde::Serialize)]
struct Request {
    sources: Vec<String>,
    source_kinds: Vec<u8>,
    result_count: usize,
    inferred_count: usize,
    language_features: Vec<String>,
    java_sources: Vec<String>,
    classpath: Option<Vec<PathBuf>>,
}

fn main() {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    let mut module_roots = Vec::new();
    let mut dep_roots = Vec::new();
    let mut cp_dirs = Vec::new();
    let mut cp_jars = usize::MAX;
    let mut cp_only = false;
    let mut open = 1usize;
    let mut max_files = usize::MAX;
    let mut stage = 1usize;
    let mut batch = 0usize;
    let mut repeat = 1usize;

    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().expect("missing flag value");
        match argument.as_str() {
            "--module" => module_roots.push(PathBuf::from(value())),
            "--deps" => dep_roots.push(PathBuf::from(value())),
            "--cp-dir" => cp_dirs.push(PathBuf::from(value())),
            "--cp-jars" => cp_jars = value().parse().unwrap(),
            "--cp-only" => cp_only = true,
            "--open" => open = value().parse().unwrap(),
            "--max" => max_files = value().parse().unwrap(),
            "--stage" => stage = value().parse().unwrap(),
            "--batch" => batch = value().parse().unwrap(),
            "--repeat" => repeat = value().parse().unwrap(),
            other => panic!("unknown flag {other}"),
        }
    }

    let mut classpath = Vec::new();
    classpath.extend(krusty::toolchain::classpath_jars_for(""));
    if let Some(modules) = platform_jdk_modules(None) {
        classpath.push(modules);
    } else {
        eprintln!("WARNING: no JDK modules found (set JAVA_HOME)");
    }
    for dir in &cp_dirs {
        let mut jars = Vec::new();
        collect_jars(dir, &mut jars);
        jars.truncate(cp_jars);
        eprintln!("classpath: +{} jars from {}", jars.len(), dir.display());
        classpath.append(&mut jars);
    }
    eprintln!("classpath entries: {}", classpath.len());

    let baseline = rss_kb();
    eprintln!("baseline rss={} MiB", baseline / 1024);

    if cp_only {
        let started = Instant::now();
        let cp = krusty::jvm::classpath::Classpath::new(classpath);
        cp.prepare_for_source_analysis();
        eprintln!(
            "cp-only: prepare took {:.2}s, rss={} MiB (delta {} MiB)",
            started.elapsed().as_secs_f64(),
            rss_kb() / 1024,
            rss_kb().saturating_sub(baseline) / 1024
        );
        return;
    }

    let mut own_files = Vec::new();
    for root in &module_roots {
        collect_kt(root, &mut own_files);
    }
    let mut dep_files = Vec::new();
    for root in &dep_roots {
        collect_kt(root, &mut dep_files);
    }
    own_files.truncate(max_files);
    let remaining = max_files.saturating_sub(own_files.len());
    dep_files.truncate(remaining);

    let read = |path: &PathBuf| std::fs::read_to_string(path).unwrap_or_default();
    let own: Vec<String> = own_files.iter().map(read).collect();
    let deps: Vec<String> = dep_files.iter().map(read).collect();
    let own_bytes: usize = own.iter().map(String::len).sum();
    let dep_bytes: usize = deps.iter().map(String::len).sum();
    eprintln!(
        "sources: own={} files/{} KiB, deps={} files/{} KiB",
        own.len(),
        own_bytes / 1024,
        deps.len(),
        dep_bytes / 1024
    );

    let mut input = Vec::new();
    let mut requests = 0usize;
    for _ in 0..repeat {
        if batch == 0 {
            let open = open.min(own.len());
            let mut sources: Vec<String> = own[..open].to_vec();
            sources.extend_from_slice(&own[open..]);
            sources.extend_from_slice(&deps);
            let request = Request {
                source_kinds: vec![0; sources.len()],
                result_count: open,
                inferred_count: own.len(),
                language_features: Vec::new(),
                java_sources: Vec::new(),
                classpath: None,
                sources,
            };
            frame(&serde_json::to_vec(&request).unwrap(), &mut input);
            requests += 1;
        } else {
            let all: Vec<&String> = own.iter().chain(deps.iter()).collect();
            for chunk in all.chunks(batch) {
                let sources: Vec<String> = chunk.iter().map(|s| (*s).clone()).collect();
                let request = Request {
                    source_kinds: vec![0; sources.len()],
                    result_count: sources.len(),
                    inferred_count: sources.len(),
                    language_features: Vec::new(),
                    java_sources: Vec::new(),
                    classpath: None,
                    sources,
                };
                frame(&serde_json::to_vec(&request).unwrap(), &mut input);
                requests += 1;
            }
        }
    }
    eprintln!(
        "requests: {} ({} KiB framed input)",
        requests,
        input.len() / 1024
    );

    let started = Instant::now();
    let mut sink = StagingSink {
        frames: 0,
        stage,
        bytes: 0,
        started,
    };
    let mut reader = Cursor::new(input);
    run_analysis_worker(&mut reader, &mut sink, classpath).expect("worker failed");
    let elapsed = started.elapsed().as_secs_f64();
    let final_rss = rss_kb();
    eprintln!(
        "done: {} responses in {:.2}s ({:.1} ms/request), rss={} MiB (delta {} MiB), wire out={} KiB",
        sink.frames.saturating_sub(1),
        elapsed,
        elapsed * 1000.0 / requests.max(1) as f64,
        final_rss / 1024,
        final_rss.saturating_sub(baseline) / 1024,
        sink.bytes / 1024
    );
}
