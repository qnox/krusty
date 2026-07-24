//! Restartable compiler-analysis worker.
//!
//! The compiler uses process-lifetime type/name interners. Keeping it in the LSP supervisor would
//! make unique types introduced by edits accumulate for the editor's entire lifetime. The worker
//! amortizes classpath startup across a bounded number of analyses, then restarts to release all
//! compiler-global memory while the supervisor retains only source text and compact query indexes.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use krusty::diag::{Diagnostic, Severity, Span};
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use serde::{Deserialize, Serialize};

use crate::compiler_analysis::{self, CompletionSymbols, DefinitionSymbols, HighlightSymbols};
use crate::{
    read_framed, write_framed, AnalysisBudgets, CompletionIndex, DefinitionIndex, DocumentAnalysis,
    HoverIndex, SemanticTokenIndex, SourceSetIndexes,
};

pub const DEFAULT_ANALYSES_PER_WORKER: usize = 64;
const MAX_WORKER_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SOURCE_SET_BYTES: usize = 32 * 1024 * 1024;
const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize)]
struct AnalysisRequest<'a> {
    sources: &'a [&'a str],
}

#[derive(Deserialize)]
struct OwnedAnalysisRequest {
    sources: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct WireDiagnostic {
    lo: u32,
    hi: u32,
    severity: u8,
    message: String,
}

#[derive(Deserialize, Serialize)]
struct AnalysisResponse {
    diagnostics: Vec<WireDiagnostic>,
    hover: HoverIndex,
    completion: CompletionIndex,
    semantic_tokens: SemanticTokenIndex,
    definitions: DefinitionIndex,
}

impl From<DocumentAnalysis> for AnalysisResponse {
    fn from(analysis: DocumentAnalysis) -> Self {
        Self {
            diagnostics: analysis
                .diagnostics
                .into_iter()
                .map(|diagnostic| WireDiagnostic {
                    lo: diagnostic.span.lo,
                    hi: diagnostic.span.hi,
                    severity: match diagnostic.severity {
                        Severity::Error => 1,
                        Severity::Warning => 2,
                    },
                    message: diagnostic.msg,
                })
                .collect(),
            hover: analysis.hover,
            completion: analysis.completion,
            semantic_tokens: analysis.semantic_tokens,
            definitions: analysis.definitions,
        }
    }
}

impl AnalysisResponse {
    fn into_document_analysis(self) -> DocumentAnalysis {
        DocumentAnalysis {
            diagnostics: self
                .diagnostics
                .into_iter()
                .map(|diagnostic| Diagnostic {
                    span: Span::new(diagnostic.lo, diagnostic.hi),
                    severity: if diagnostic.severity == 2 {
                        Severity::Warning
                    } else {
                        Severity::Error
                    },
                    msg: diagnostic.message,
                    file: 0,
                })
                .collect(),
            hover: self.hover,
            completion: self.completion,
            semantic_tokens: self.semantic_tokens,
            definitions: self.definitions,
        }
    }
}

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Option<BufReader<ChildStdout>>,
}

struct BoundedVec {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedVec {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
}

impl Write for BoundedVec {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "analysis message exceeds worker limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn source_set_fits(lengths: impl IntoIterator<Item = usize>) -> bool {
    lengths
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .is_some_and(|total| total <= MAX_SOURCE_SET_BYTES)
}

fn encode_request(sources: &[&str]) -> io::Result<Vec<u8>> {
    if !source_set_fits(sources.iter().map(|source| source.len())) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "open source set exceeds analysis limit",
        ));
    }
    let mut request = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
    serde_json::to_writer(&mut request, &AnalysisRequest { sources })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(request.bytes)
}

fn encode_response(analyses: &[AnalysisResponse]) -> io::Result<Vec<u8>> {
    let mut response = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
    serde_json::to_writer(&mut response, analyses)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(response.bytes)
}

impl WorkerProcess {
    fn spawn(executable: &Path, arguments: &[String]) -> io::Result<Self> {
        let mut child = Command::new(executable)
            .arg("--analysis-worker")
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("analysis worker stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("analysis worker stdout unavailable"))?;
        Ok(Self {
            child,
            stdin,
            stdout: Some(BufReader::new(stdout)),
        })
    }

    fn read_response(&mut self) -> io::Result<Vec<u8>> {
        let mut stdout = self
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("analysis worker stdout unavailable"))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let response = read_framed(&mut stdout, MAX_WORKER_MESSAGE_BYTES);
            let _ = sender.send((stdout, response));
        });
        match receiver.recv_timeout(ANALYSIS_TIMEOUT) {
            Ok((stdout, response)) => {
                self.stdout = Some(stdout);
                response?.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "analysis worker exited")
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                if let Ok((stdout, _)) = receiver.recv() {
                    self.stdout = Some(stdout);
                }
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "analysis worker timed out",
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "analysis worker response reader stopped",
            )),
        }
    }

    fn analyze(&mut self, sources: &[&str]) -> io::Result<Vec<DocumentAnalysis>> {
        let request = encode_request(sources)?;
        write_framed(&mut self.stdin, &request)?;
        drop(request);
        let response = self.read_response()?;
        let analyses =
            serde_json::from_slice::<Vec<AnalysisResponse>>(&response).map_err(json_io)?;
        drop(response);
        Ok(analyses
            .into_iter()
            .map(AnalysisResponse::into_document_analysis)
            .collect())
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct AnalysisWorker {
    executable: PathBuf,
    arguments: Vec<String>,
    process: WorkerProcess,
    analyses: usize,
    max_analyses: usize,
}

impl AnalysisWorker {
    pub fn spawn(executable: PathBuf, arguments: Vec<String>) -> io::Result<Self> {
        let process = WorkerProcess::spawn(&executable, &arguments)?;
        Ok(Self {
            executable,
            arguments,
            process,
            analyses: 0,
            max_analyses: DEFAULT_ANALYSES_PER_WORKER,
        })
    }

    fn restart(&mut self) -> io::Result<()> {
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
        let replacement = WorkerProcess::spawn(&self.executable, &self.arguments)?;
        self.process = replacement;
        self.analyses = 0;
        Ok(())
    }

    pub fn analyze(&mut self, sources: &[&str]) -> io::Result<Vec<DocumentAnalysis>> {
        if self.analyses >= self.max_analyses {
            self.restart()?;
        }
        match self.process.analyze(sources) {
            Ok(analysis) => {
                self.analyses += 1;
                Ok(analysis)
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Err(error),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                self.restart()?;
                Err(error)
            }
            Err(_) => {
                self.restart()?;
                let analysis = self.process.analyze(sources)?;
                self.analyses += 1;
                Ok(analysis)
            }
        }
    }
}

pub fn run_analysis_worker<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    classpath: Vec<PathBuf>,
) -> io::Result<()> {
    let classpath = Rc::new(Classpath::new(classpath));
    while let Some(body) = read_framed(reader, MAX_WORKER_MESSAGE_BYTES)? {
        let request: OwnedAnalysisRequest = serde_json::from_slice(&body).map_err(json_io)?;
        drop(body);
        let sources: Vec<_> = request.sources.iter().map(String::as_str).collect();
        let platform = Box::new(JvmLibraries::new(classpath.clone()));
        let source_set = compiler_analysis::analyze_source_set(&sources, platform);
        let highlight_symbols =
            HighlightSymbols::from_source_set(&sources, &source_set.files, &source_set.symbols);
        let definition_symbols =
            DefinitionSymbols::from_source_set(&sources, &source_set.files, &source_set.symbols);
        let completion_symbols = CompletionSymbols::from_source_set(&sources, &source_set.files);
        let indexes = SourceSetIndexes::new(
            &source_set.symbols,
            &highlight_symbols,
            &definition_symbols,
            &completion_symbols,
        );
        let mut budgets = AnalysisBudgets::new();
        let analyses = source_set
            .files
            .into_iter()
            .zip(&sources)
            .enumerate()
            .map(|(file_index, (file, source))| {
                DocumentAnalysis::from_file_analysis(
                    source,
                    file,
                    file_index as u32,
                    &indexes,
                    &mut budgets,
                )
            })
            .map(AnalysisResponse::from)
            .collect::<Vec<_>>();
        let response = encode_response(&analyses)?;
        write_framed(writer, &response)?;
    }
    Ok(())
}

fn json_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn source_and_wire_buffers_are_bounded_before_worker_io() {
        assert!(source_set_fits([MAX_SOURCE_SET_BYTES]));
        assert!(!source_set_fits([MAX_SOURCE_SET_BYTES, 1]));

        let mut output = BoundedVec::new(4);
        output.write_all(b"1234").unwrap();
        let error = output.write_all(b"5").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(output.bytes, b"1234");
    }

    #[test]
    fn worker_protocol_analyzes_a_cross_file_source_set() {
        let sources = [
            "package demo\nfun answer(): Int = 42",
            "package demo\nfun use(): Int = answer()",
        ];
        let request = serde_json::to_vec(&AnalysisRequest { sources: &sources }).unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();
        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let response = read_framed(&mut Cursor::new(output), MAX_WORKER_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        let analyses = serde_json::from_slice::<Vec<AnalysisResponse>>(&response).unwrap();
        let analysis = analyses
            .into_iter()
            .nth(1)
            .unwrap()
            .into_document_analysis();
        assert!(analysis.diagnostics.is_empty());
        assert!(analysis.hover.entry_count() > 0);
        assert!(analysis.completion.entry_count() > 0);
        assert!(analysis.semantic_tokens.entry_count() > 0);
        assert!(analysis.definitions.entry_count() > 0);
    }
}
