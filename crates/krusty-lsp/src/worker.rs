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

use krusty::diag::{Diagnostic, DiagnosticKind, Severity, Span};
use krusty::features::LangFeatures;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::source::{SourceInput, SourceKind};
use serde::{Deserialize, Serialize};

use crate::compiler_analysis::{
    self, CompletionSymbols, DefinitionSymbols, HighlightSymbols, LibraryRef, SignatureHelpSymbols,
};
use crate::{
    finalize_navigation, read_framed, write_framed, AnalysisBudgets, CompletionIndex,
    DefinitionIndex, DocumentAnalysis, DocumentSymbolIndex, FoldingRangeIndex, HoverIndex,
    LibraryDefinitionIndex, SemanticTokenIndex, SignatureHelpIndex, SourceSetIndexes,
    WorkspaceSymbolIndex,
};

pub const DEFAULT_ANALYSES_PER_WORKER: usize = 64;
const MAX_WORKER_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SOURCE_SET_BYTES: usize = 32 * 1024 * 1024;
const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const READINESS_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const WORKER_READY: &[u8] = b"ready";

#[derive(Serialize)]
struct AnalysisRequest<'a> {
    sources: &'a [&'a str],
    source_kinds: &'a [u8],
    result_count: usize,
    inferred_count: usize,
    language_features: &'a [&'a str],
    java_sources: &'a [String],
    classpath: Option<&'a [PathBuf]>,
}

#[derive(Deserialize)]
struct OwnedAnalysisRequest {
    sources: Vec<String>,
    #[serde(default)]
    source_kinds: Vec<u8>,
    result_count: usize,
    #[serde(default)]
    inferred_count: Option<usize>,
    #[serde(default)]
    language_features: Vec<String>,
    #[serde(default)]
    java_sources: Vec<String>,
    #[serde(default)]
    classpath: Option<Vec<PathBuf>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OwnedWorkerRequest {
    Materialize {
        materialize: OwnedMaterializeRequest,
    },
    Dump {
        dump: OwnedDumpRequest,
    },
    Analyze(OwnedAnalysisRequest),
}

#[derive(Deserialize, Serialize)]
struct OwnedMaterializeRequest {
    reference: LibraryRef,
    use_sources: bool,
}

#[derive(Deserialize, Serialize)]
struct MaterializeResponse {
    text: String,
    lo: u32,
    hi: u32,
}

/// A dump request as the worker receives it: an analysis payload plus which of its files to render.
#[derive(Deserialize)]
struct OwnedDumpRequest {
    analysis: OwnedAnalysisRequest,
    target: usize,
    label: String,
    cache_key: String,
    cache_root: PathBuf,
}

/// A dump request as the supervisor sends it, borrowing the retained source texts.
#[derive(Serialize)]
struct DumpRequest<'a> {
    analysis: AnalysisRequest<'a>,
    target: usize,
    label: &'a str,
    cache_key: &'a str,
    cache_root: &'a Path,
}

/// The tagged wrapper that routes a `DumpRequest` to the worker's dump arm.
///
/// A typed envelope rather than `serde_json::json!`: the macro builds a fully owned `Value` copy of
/// every source text before a byte reaches the bounded writer, and its internal `unwrap` would turn
/// a non-UTF-8 `cache_root` — legal on Linux and macOS — into a panic in the supervisor process.
#[derive(Serialize)]
struct DumpEnvelope<'a> {
    dump: &'a DumpRequest<'a>,
}

/// Everything a dump needs from the session that produced the retained analysis payload.
///
/// The fields mirror `analyze_inputs_prefix_with_config`, because a dump is only trustworthy if the
/// worker re-analyzes under exactly the configuration the editor's own analysis used: a per-module
/// classpath, that module's language arguments, and the group's Java sources all change what
/// resolves. A dump that silently analyzes against the launch classpath would report unresolved
/// types for a file the editor shows as clean.
pub struct DumpTarget<'a> {
    /// Retained source texts, in the order the session analyzed them.
    pub sources: &'a [String],
    /// Kinds parallel to `sources`. Empty means every source is Kotlin.
    pub source_kinds: &'a [SourceKind],
    /// Index into `sources` of the file to render.
    pub target: usize,
    /// Workspace-relative presentation label shown in the dump heading.
    pub label: &'a str,
    /// Full document identity used only as opaque input to the cache digest. It is deliberately
    /// separate from `label`: same-named files outside the workspace must not overwrite each other,
    /// and the URI must never be copied into a readable cache filename.
    pub cache_key: &'a str,
    pub cache_root: &'a Path,
    /// The checked and inference prefixes the session used.
    pub result_count: usize,
    pub inferred_count: usize,
    /// Java texts the analysis stubs onto the classpath.
    pub java_sources: &'a [String],
    /// Per-module language CLI arguments; `None` keeps the worker's session features.
    pub language_arguments: Option<&'a [String]>,
    /// Per-module classpath; `None` keeps the worker's launch classpath.
    pub classpath: Option<&'a [PathBuf]>,
}

/// Where the worker wrote a rendered dump.
///
/// Only the path crosses the channel: a dump document carries every AST node, typed expression, and
/// IR instruction of a file, which would put the response at risk of the worker frame cap and then
/// the LSP message cap.
#[derive(Deserialize, Serialize)]
pub struct DumpResponse {
    pub path: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireDiagnostic {
    lo: u32,
    hi: u32,
    severity: u8,
    kind: u8,
    message: String,
}

#[derive(Deserialize, Serialize)]
struct AnalysisResponse {
    diagnostics: Vec<WireDiagnostic>,
    hover: HoverIndex,
    completion: CompletionIndex,
    signature_help: SignatureHelpIndex,
    semantic_tokens: SemanticTokenIndex,
    definitions: DefinitionIndex,
    type_definitions: DefinitionIndex,
    implementations: DefinitionIndex,
    #[serde(default)]
    library_definitions: LibraryDefinitionIndex,
    document_symbols: DocumentSymbolIndex,
    #[serde(default)]
    workspace_symbols: WorkspaceSymbolIndex,
    folding_ranges: FoldingRangeIndex,
    #[serde(default)]
    implementation_relations: Vec<[u32; 6]>,
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
                    kind: match diagnostic.kind {
                        DiagnosticKind::Compiler => 0,
                        DiagnosticKind::IncompatibleEquality => 1,
                        DiagnosticKind::Inspection => 2,
                    },
                    message: diagnostic.msg,
                })
                .collect(),
            hover: analysis.hover,
            completion: analysis.completion,
            signature_help: analysis.signature_help,
            semantic_tokens: analysis.semantic_tokens,
            definitions: analysis.definitions,
            type_definitions: analysis.type_definitions,
            implementations: analysis.implementations,
            library_definitions: analysis.library_definitions,
            document_symbols: analysis.document_symbols,
            workspace_symbols: analysis.workspace_symbols,
            folding_ranges: analysis.folding_ranges,
            implementation_relations: analysis.implementation_relations,
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
                    editor_span: None,
                    identity: None,
                    severity: if diagnostic.severity == 2 {
                        Severity::Warning
                    } else {
                        Severity::Error
                    },
                    kind: match diagnostic.kind {
                        1 => DiagnosticKind::IncompatibleEquality,
                        2 => DiagnosticKind::Inspection,
                        _ => DiagnosticKind::Compiler,
                    },
                    msg: diagnostic.message,
                    file: 0,
                })
                .collect(),
            hover: self.hover,
            completion: self.completion,
            signature_help: self.signature_help,
            semantic_tokens: self.semantic_tokens,
            definitions: self.definitions,
            type_definitions: self.type_definitions,
            implementations: self.implementations,
            library_definitions: self.library_definitions,
            document_symbols: self.document_symbols,
            workspace_symbols: self.workspace_symbols,
            folding_ranges: self.folding_ranges,
            implementation_relations: self.implementation_relations,
        }
    }
}

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Option<BufReader<ChildStdout>>,
}

/// Borrowed send shape: a many-thousand-entry classpath must stream into the bounded writer without
/// first cloning every `PathBuf`. The owning receive shape below is deliberately separate because the
/// worker must retain the decoded paths after the launch frame is dropped.
#[derive(Serialize)]
struct WorkerLaunchConfiguration<'a> {
    classpath: &'a [PathBuf],
}

#[derive(Deserialize)]
struct OwnedWorkerLaunchConfiguration {
    classpath: Vec<PathBuf>,
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

pub fn source_set_fits(lengths: impl IntoIterator<Item = usize>) -> bool {
    lengths
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .is_some_and(|total| total <= MAX_SOURCE_SET_BYTES)
}

fn encode_request(
    inputs: &[SourceInput<'_>],
    result_count: usize,
    inferred_count: usize,
    features: &LangFeatures,
    java_sources: &[String],
    classpath: Option<&[PathBuf]>,
) -> io::Result<Vec<u8>> {
    if !source_set_fits(
        inputs
            .iter()
            .map(|source| source.text.len())
            .chain(java_sources.iter().map(String::len)),
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "open source set exceeds analysis limit",
        ));
    }
    if result_count > inferred_count || inferred_count > inputs.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "analysis result and inference prefixes do not align with the source count",
        ));
    }
    let sources = inputs.iter().map(|source| source.text).collect::<Vec<_>>();
    let source_kinds = inputs
        .iter()
        .map(|source| source.kind.wire_code())
        .collect::<Vec<_>>();
    let mut request = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
    let mut language_features = features.iter().collect::<Vec<_>>();
    language_features.sort_unstable();
    serde_json::to_writer(
        &mut request,
        &AnalysisRequest {
            sources: &sources,
            source_kinds: &source_kinds,
            result_count,
            inferred_count,
            language_features: &language_features,
            java_sources,
            classpath,
        },
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(request.bytes)
}

fn encode_response(analyses: &[AnalysisResponse]) -> io::Result<Vec<u8>> {
    let mut response = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
    serde_json::to_writer(&mut response, analyses)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(response.bytes)
}

fn encode_launch_configuration(classpath: &[PathBuf]) -> io::Result<Vec<u8>> {
    let mut configuration = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
    serde_json::to_writer(&mut configuration, &WorkerLaunchConfiguration { classpath })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(configuration.bytes)
}

fn response_wire_bytes(analyses: &[AnalysisResponse]) -> io::Result<usize> {
    crate::analysis::serialized_json_wire_bytes(analyses).map_err(json_io)
}

fn encode_materialize_request(reference: &LibraryRef, use_sources: bool) -> io::Result<Vec<u8>> {
    let mut request = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
    serde_json::to_writer(
        &mut request,
        &serde_json::json!({
            "materialize": OwnedMaterializeRequest {
                reference: reference.clone(),
                use_sources,
            }
        }),
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(request.bytes)
}

/// Encode a dump request, carrying the module configuration through unchanged.
///
/// `session_features` applies only when the target names no language arguments of its own, mirroring
/// `analyze_inputs_prefix` (session features) versus `analyze_inputs_prefix_with_config` (the
/// module's own arguments).
fn encode_dump_request(
    target: &DumpTarget<'_>,
    session_features: &LangFeatures,
) -> io::Result<Vec<u8>> {
    let sources = target
        .sources
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let source_kinds = target
        .source_kinds
        .iter()
        .map(|kind| kind.wire_code())
        .collect::<Vec<_>>();
    let features = match target.language_arguments {
        Some(arguments) => {
            let mut features = LangFeatures::new();
            for argument in arguments {
                features.apply_cli_arg(argument);
            }
            features
        }
        None => session_features.clone(),
    };
    let mut language_features = features.iter().collect::<Vec<_>>();
    language_features.sort_unstable();
    let request = DumpRequest {
        analysis: AnalysisRequest {
            sources: &sources,
            source_kinds: &source_kinds,
            result_count: target.result_count,
            inferred_count: target.inferred_count,
            language_features: &language_features,
            java_sources: target.java_sources,
            classpath: target.classpath,
        },
        target: target.target,
        label: target.label,
        cache_key: target.cache_key,
        cache_root: target.cache_root,
    };
    let mut encoded = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
    serde_json::to_writer(&mut encoded, &DumpEnvelope { dump: &request })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(encoded.bytes)
}

fn framed_read_receiver<R>(
    mut reader: R,
    max_bytes: usize,
) -> mpsc::Receiver<(R, io::Result<Option<Vec<u8>>>)>
where
    R: BufRead + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let response = read_framed(&mut reader, max_bytes);
        let _ = sender.send((reader, response));
    });
    receiver
}

impl WorkerProcess {
    fn spawn(executable: &Path, classpath: &[PathBuf]) -> io::Result<Self> {
        let configuration = encode_launch_configuration(classpath)?;
        let mut child = Command::new(executable)
            .arg("--analysis-worker")
            // Supply the identity rather than making the child discover it after exec. If the
            // server is killed in that interval, getppid() already names the reaper and a worker
            // that sampled only its current parent could mistake that process for its server.
            .arg(std::process::id().to_string())
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
        let mut process = Self {
            child,
            stdin,
            stdout: Some(BufReader::new(stdout)),
        };
        write_framed(&mut process.stdin, &configuration)?;
        process.wait_until_ready()?;
        Ok(process)
    }

    fn wait_until_ready(&mut self) -> io::Result<()> {
        let ready = self
            .read_frame(
                WORKER_READY.len(),
                READINESS_TIMEOUT,
                "analysis worker readiness timed out",
            )?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "analysis worker exited")
            })?;
        if ready != WORKER_READY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "analysis worker sent an invalid readiness message",
            ));
        }
        Ok(())
    }

    fn read_frame(
        &mut self,
        max_bytes: usize,
        timeout: Duration,
        timeout_message: &'static str,
    ) -> io::Result<Option<Vec<u8>>> {
        let stdout = self
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("analysis worker stdout unavailable"))?;
        let receiver = framed_read_receiver(stdout, max_bytes);
        match receiver.recv_timeout(timeout) {
            Ok((stdout, response)) => {
                self.stdout = Some(stdout);
                response
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                if let Ok((stdout, _)) = receiver.recv() {
                    self.stdout = Some(stdout);
                }
                Err(io::Error::new(io::ErrorKind::TimedOut, timeout_message))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "analysis worker response reader stopped",
            )),
        }
    }

    fn read_response(&mut self) -> io::Result<Vec<u8>> {
        match self.read_frame(
            MAX_WORKER_MESSAGE_BYTES,
            ANALYSIS_TIMEOUT,
            "analysis worker timed out",
        )? {
            Some(response) => Ok(response),
            None => match self.child.wait() {
                Ok(status) if status.success() => Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "analysis worker classpath changed",
                )),
                Ok(status) => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("analysis worker exited with {status}"),
                )),
                Err(error) => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("analysis worker exited: {error}"),
                )),
            },
        }
    }

    fn analyze(
        &mut self,
        inputs: &[SourceInput<'_>],
        result_count: usize,
        inferred_count: usize,
        language_features: &LangFeatures,
        java_sources: &[String],
        classpath: Option<&[PathBuf]>,
    ) -> io::Result<Vec<DocumentAnalysis>> {
        let request = encode_request(
            inputs,
            result_count,
            inferred_count,
            language_features,
            java_sources,
            classpath,
        )?;
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

    fn materialize(
        &mut self,
        reference: &LibraryRef,
        use_sources: bool,
    ) -> io::Result<Option<MaterializeResponse>> {
        let request = encode_materialize_request(reference, use_sources)?;
        write_framed(&mut self.stdin, &request)?;
        let response = self.read_response()?;
        serde_json::from_slice(&response).map_err(json_io)
    }

    fn dump(&mut self, request: &[u8]) -> io::Result<Option<DumpResponse>> {
        write_framed(&mut self.stdin, request)?;
        let response = self.read_response()?;
        serde_json::from_slice(&response).map_err(json_io)
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
    classpath: Vec<PathBuf>,
    process: WorkerProcess,
    restart_required: bool,
    analyses: usize,
    max_analyses: usize,
    language_features: LangFeatures,
}

impl AnalysisWorker {
    pub fn spawn(executable: PathBuf, classpath: Vec<PathBuf>) -> io::Result<Self> {
        let process = WorkerProcess::spawn(&executable, &classpath)?;
        Ok(Self {
            executable,
            classpath,
            process,
            restart_required: false,
            analyses: 0,
            max_analyses: DEFAULT_ANALYSES_PER_WORKER,
            language_features: LangFeatures::new(),
        })
    }

    fn restart(&mut self) -> io::Result<()> {
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
        self.restart_required = true;
        let replacement = WorkerProcess::spawn(&self.executable, &self.classpath)?;
        self.process = replacement;
        self.restart_required = false;
        self.analyses = 0;
        Ok(())
    }

    /// Point the worker at a new classpath and JDK, restarting it so the change takes effect.
    ///
    /// The worker interns compiler-global types for its whole lifetime, so this cannot be applied in
    /// place — the process is replaced, which is exactly the bounded-restart path the supervisor
    /// already relies on. The parent resolves the complete classpath from the project entries and
    /// JDK, then sends it through the replacement worker's framed stdin. When nothing would change,
    /// the worker is left running.
    pub fn reconfigure(
        &mut self,
        classpath: &[PathBuf],
        jdk_home: Option<&Path>,
        no_jdk: bool,
    ) -> io::Result<()> {
        let classpath = crate::options::effective_classpath_for(classpath, jdk_home, no_jdk);
        if classpath == self.classpath {
            return if self.restart_required {
                self.restart()
            } else {
                Ok(())
            };
        }
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
        self.restart_required = true;
        let replacement = WorkerProcess::spawn(&self.executable, &classpath)?;
        self.classpath = classpath;
        self.process = replacement;
        self.restart_required = false;
        self.analyses = 0;
        Ok(())
    }

    pub fn analyze(&mut self, sources: &[&str]) -> io::Result<Vec<DocumentAnalysis>> {
        let inputs = sources
            .iter()
            .map(|source| SourceInput::kotlin(source))
            .collect::<Vec<_>>();
        self.analyze_inputs_prefix(&inputs, sources.len(), sources.len(), &[])
    }

    pub fn analyze_inputs_prefix(
        &mut self,
        inputs: &[SourceInput<'_>],
        result_count: usize,
        inferred_count: usize,
        java_sources: &[String],
    ) -> io::Result<Vec<DocumentAnalysis>> {
        let features = self.language_features.clone();
        self.request(|process| {
            process.analyze(
                inputs,
                result_count,
                inferred_count,
                &features,
                java_sources,
                None,
            )
        })
    }

    pub fn analyze_inputs_prefix_with_config(
        &mut self,
        inputs: &[SourceInput<'_>],
        result_count: usize,
        inferred_count: usize,
        java_sources: &[String],
        language_arguments: &[String],
        classpath: Option<&[PathBuf]>,
    ) -> io::Result<Vec<DocumentAnalysis>> {
        let mut features = LangFeatures::new();
        for argument in language_arguments {
            features.apply_cli_arg(argument);
        }
        self.request(|process| {
            process.analyze(
                inputs,
                result_count,
                inferred_count,
                &features,
                java_sources,
                classpath,
            )
        })
    }

    pub fn materialize_library_definition(
        &mut self,
        reference: &LibraryRef,
        use_sources: bool,
    ) -> io::Result<Option<(String, Span)>> {
        self.request(|process| process.materialize(reference, use_sources))
            .map(|response| {
                response.map(|response| (response.text, Span::new(response.lo, response.hi)))
            })
    }

    /// Render the dump for `target.sources[target.target]` in the worker and return where it landed.
    ///
    /// The whole `DumpTarget` is replayed: the prefixes keep the dump's view of the source set equal
    /// to the session's, and the module classpath, language arguments, and Java sources keep its
    /// view of what resolves equal too.
    pub fn dump(&mut self, target: &DumpTarget<'_>) -> io::Result<Option<DumpResponse>> {
        let request = encode_dump_request(target, &self.language_features)?;
        self.request(|process| process.dump(&request))
    }

    fn request<T>(
        &mut self,
        mut operation: impl FnMut(&mut WorkerProcess) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.restart_required || self.analyses >= self.max_analyses {
            self.restart()?;
        }
        match operation(&mut self.process) {
            Ok(result) => {
                self.analyses += 1;
                Ok(result)
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Err(error),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                self.restart_required = true;
                Err(error)
            }
            Err(_) => {
                self.restart()?;
                match operation(&mut self.process) {
                    Ok(result) => {
                        self.analyses += 1;
                        Ok(result)
                    }
                    Err(error) => {
                        self.restart_required = true;
                        Err(error)
                    }
                }
            }
        }
    }

    pub fn set_language_features(&mut self, features: LangFeatures) {
        self.language_features = features;
    }
}

fn compact_implementation_relations(
    relations: impl IntoIterator<
        Item = (
            crate::compiler_analysis::DefinitionTarget,
            crate::compiler_analysis::DefinitionTarget,
        ),
    >,
) -> Vec<[u32; 6]> {
    let mut compact = relations
        .into_iter()
        .map(|(declaration, implementation)| {
            [
                declaration.file,
                declaration.span.lo,
                declaration.span.hi,
                implementation.file,
                implementation.span.lo,
                implementation.span.hi,
            ]
        })
        .collect::<Vec<_>>();
    compact.sort_unstable();
    compact.dedup();
    compact
}

fn retain_implementation_relations_for_response(
    analyses: &mut [AnalysisResponse],
    mut relations: Vec<[u32; 6]>,
    max_navigation_entries: usize,
    max_wire_bytes: usize,
) -> io::Result<()> {
    let retained_navigation_entries = analyses.iter().fold(0usize, |total, analysis| {
        total
            .saturating_add(analysis.definitions.entry_count())
            .saturating_add(analysis.type_definitions.entry_count())
            .saturating_add(analysis.implementations.entry_count())
    });
    relations.truncate(max_navigation_entries.saturating_sub(retained_navigation_entries));
    let baseline_bytes = response_wire_bytes(analyses)?;
    if baseline_bytes > max_wire_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained analysis exceeds worker response limit",
        ));
    }
    if analyses.is_empty() || relations.is_empty() {
        return Ok(());
    }

    let mut remaining_wire_bytes = max_wire_bytes.saturating_sub(baseline_bytes);
    let mut retained = 0usize;
    for relation in &relations {
        let encoded_bytes =
            crate::analysis::serialized_json_wire_bytes(relation).map_err(json_io)?;
        let wire_bytes = encoded_bytes.saturating_add(usize::from(retained > 0));
        if wire_bytes > remaining_wire_bytes {
            break;
        }
        remaining_wire_bytes -= wire_bytes;
        retained += 1;
    }
    relations.truncate(retained);
    analyses[0].implementation_relations = relations;
    Ok(())
}

pub fn run_analysis_worker<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    classpath: Vec<PathBuf>,
) -> io::Result<()> {
    let default_classpath = Rc::new(Classpath::new(classpath));
    default_classpath.prepare_for_source_analysis();
    write_framed(writer, WORKER_READY)?;
    while let Some(body) = read_framed(reader, MAX_WORKER_MESSAGE_BYTES)? {
        let request: OwnedWorkerRequest = serde_json::from_slice(&body).map_err(json_io)?;
        drop(body);
        let request = match request {
            OwnedWorkerRequest::Analyze(request) => request,
            OwnedWorkerRequest::Materialize { materialize } => {
                let response = crate::dependency_sources::render::materialize(
                    &default_classpath,
                    &materialize.reference.fqn,
                    &materialize.reference.member_name,
                    &materialize.reference.member_desc,
                    materialize.use_sources,
                )
                .map(|source| {
                    let (text, span) = source.into_text_and_span(
                        &materialize.reference.member_name,
                        &materialize.reference.member_desc,
                    );
                    MaterializeResponse {
                        text,
                        lo: span.lo,
                        hi: span.hi,
                    }
                });
                let mut encoded = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
                serde_json::to_writer(&mut encoded, &response).map_err(json_io)?;
                write_framed(writer, &encoded.bytes)?;
                continue;
            }
            OwnedWorkerRequest::Dump { dump } => {
                let response = render_dump_request(&default_classpath, dump);
                let mut encoded = BoundedVec::new(MAX_WORKER_MESSAGE_BYTES);
                serde_json::to_writer(&mut encoded, &response).map_err(json_io)?;
                write_framed(writer, &encoded.bytes)?;
                continue;
            }
        };
        let inferred_count = request.inferred_count.unwrap_or(request.sources.len());
        if request.result_count > inferred_count || inferred_count > request.sources.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "analysis result and inference prefixes do not align with the source count",
            ));
        }
        if !source_set_fits(request.sources.iter().map(String::len)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "analysis source set exceeds size limit",
            ));
        }
        let source_kinds = if request.source_kinds.is_empty() {
            vec![0; request.sources.len()]
        } else {
            request.source_kinds
        };
        if source_kinds.len() != request.sources.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "analysis source kinds do not align with source texts",
            ));
        }
        let inputs = request
            .sources
            .iter()
            .zip(source_kinds)
            .map(|(source, kind)| {
                let kind = SourceKind::from_wire_code(kind).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "analysis request contains an unknown source kind",
                    )
                })?;
                Ok(SourceInput::new(kind, source))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let sources = inputs.iter().map(|input| input.text).collect::<Vec<_>>();
        let java_documents = inputs
            .iter()
            .enumerate()
            .filter(|(_, input)| input.kind == SourceKind::Java)
            .map(|(index, _)| index as u32)
            .collect::<Vec<_>>();
        let mut language_features = LangFeatures::new();
        for feature in &request.language_features {
            language_features.enable(feature);
        }
        let classpath = request.classpath.as_ref().map_or_else(
            || default_classpath.clone(),
            |entries| {
                let classpath = Rc::new(Classpath::new(entries.clone()));
                classpath.prepare_for_source_analysis();
                classpath
            },
        );
        let stub_overlay_set = set_java_stub_overlay(&classpath, &request.java_sources);
        let platform = Box::new(JvmLibraries::new(classpath.clone()));
        let source_set = compiler_analysis::analyze_source_inputs_prefix_with_features(
            &inputs,
            request.result_count,
            inferred_count,
            platform,
            &language_features,
        );
        let highlight_symbols =
            HighlightSymbols::from_source_set(&source_set.files, &source_set.symbols);
        let mut definition_symbols = DefinitionSymbols::from_source_set(
            &sources,
            &source_set.files,
            &source_set.symbols,
            crate::analysis::MAX_SOURCE_SET_NAVIGATION_ENTRIES,
        );
        crate::analysis::register_java_declarations(
            &mut definition_symbols,
            &sources,
            &java_documents,
        );
        let completion_symbols =
            CompletionSymbols::from_source_set_prefix(&source_set.files, inferred_count);
        let signature_help_symbols =
            SignatureHelpSymbols::from_source_set(&sources, &source_set.files, &source_set.symbols);
        let workspace_symbols = WorkspaceSymbolIndex::from_source_set(&sources, &source_set.files);
        let indexes = SourceSetIndexes::new(
            &source_set.symbols,
            &highlight_symbols,
            &definition_symbols,
            &completion_symbols,
            &signature_help_symbols,
        );
        let mut budgets = AnalysisBudgets::new();
        let pending = source_set
            .files
            .into_iter()
            .zip(&sources)
            .enumerate()
            .take(request.result_count)
            .map(|(file_index, (file, source))| {
                DocumentAnalysis::from_file_analysis(
                    source,
                    file,
                    file_index as u32,
                    &indexes,
                    &mut budgets,
                )
            })
            .collect();
        let implementation_relations =
            compact_implementation_relations(definition_symbols.implementation_relations());
        let mut analyses = finalize_navigation(pending, &mut budgets);
        crate::analysis::apply_java_navigation(
            &mut analyses,
            &sources,
            &java_documents,
            &definition_symbols,
            &mut budgets,
        );
        if let Some(first) = analyses.first_mut() {
            first.workspace_symbols = workspace_symbols;
        }
        crate::retain_analysis_wire_budget(&mut analyses, MAX_WORKER_MESSAGE_BYTES);
        let mut analyses = analyses
            .into_iter()
            .map(AnalysisResponse::from)
            .collect::<Vec<_>>();
        retain_implementation_relations_for_response(
            &mut analyses,
            implementation_relations,
            crate::analysis::MAX_SOURCE_SET_NAVIGATION_ENTRIES,
            MAX_WORKER_MESSAGE_BYTES,
        )?;
        if stub_overlay_set {
            classpath.clear_stub_overlay();
        }
        let response = encode_response(&analyses)?;
        // A clean EOF makes the supervisor retry the request in a fresh worker.
        if !classpath.snapshot_is_current() {
            return Ok(());
        }
        write_framed(writer, &response)?;
    }
    Ok(())
}

/// Read the supervisor-resolved classpath before initializing the worker.
///
/// The classpath deliberately crosses the framed stdin pipe rather than the process argument or
/// environment vectors. Both vectors share the platform's `exec` size ceiling, which large JPS
/// workspaces can exceed before the worker process starts.
pub fn run_configured_analysis_worker<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> io::Result<()> {
    let body = read_framed(reader, MAX_WORKER_MESSAGE_BYTES)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "analysis worker launch configuration missing",
        )
    })?;
    let configuration =
        serde_json::from_slice::<OwnedWorkerLaunchConfiguration>(&body).map_err(json_io)?;
    drop(body);
    run_analysis_worker(reader, writer, configuration.classpath)
}

/// Stub `java_sources` onto `classpath` so Kotlin sources resolve Java declarations that no compiled
/// class covers. Returns whether an overlay was installed, so the caller knows to clear it.
fn set_java_stub_overlay(classpath: &Classpath, java_sources: &[String]) -> bool {
    if java_sources.is_empty() {
        return false;
    }
    let java: Vec<(String, String)> = java_sources
        .iter()
        .map(|source| (String::new(), source.clone()))
        .collect();
    let resolve = |cand: &str| {
        classpath
            .find_name(krusty::types::type_name(cand))
            .is_some()
    };
    match krusty::jvm::java_stub::stub_classes(
        &java,
        krusty::jvm::java_stub::StubMode::Lenient,
        &resolve,
    ) {
        Some(stubs) => {
            classpath.set_stub_overlay(stubs);
            true
        }
        None => false,
    }
}

/// Analyze the payload, lower the target file, and store the rendered dump.
///
/// Returns `None` when the request cannot be interpreted or the dump cannot be written. Lowering
/// failures are not among those cases: the bail reason is the most valuable line in the document, so
/// it is rendered into the IR section instead of discarding the dump.
fn render_dump_request(
    default_classpath: &Rc<Classpath>,
    request: OwnedDumpRequest,
) -> Option<DumpResponse> {
    let sources = request.analysis.sources;
    if request.target >= sources.len() {
        return None;
    }
    // Kinds decode exactly as they do for analysis. Parsing a `.java` or `.kts` document as Kotlin
    // would put a garbage AST's declarations into the symbol table the dumped file is checked
    // against, so a set of kinds that cannot be trusted refuses the dump rather than guessing.
    let kinds = if request.analysis.source_kinds.is_empty() {
        vec![SourceKind::Kotlin; sources.len()]
    } else if request.analysis.source_kinds.len() == sources.len() {
        request
            .analysis
            .source_kinds
            .iter()
            .map(|code| SourceKind::from_wire_code(*code))
            .collect::<Option<Vec<_>>>()?
    } else {
        return None;
    };
    let inputs = sources
        .iter()
        .zip(&kinds)
        .map(|(source, kind)| SourceInput::new(*kind, source))
        .collect::<Vec<_>>();

    let mut features = LangFeatures::new();
    for feature in &request.analysis.language_features {
        features.enable(feature);
    }
    let classpath = request.analysis.classpath.as_ref().map_or_else(
        || default_classpath.clone(),
        |entries| {
            let classpath = Rc::new(Classpath::new(entries.clone()));
            classpath.prepare_for_source_analysis();
            classpath
        },
    );
    // Replay the prefix the session analyzed, widened when needed so the dumped file is always
    // checked — an unchecked target would render neither types nor IR.
    let result_count = request
        .analysis
        .result_count
        .max(request.target + 1)
        .min(sources.len());
    let inferred_count = request
        .analysis
        .inferred_count
        .unwrap_or(sources.len())
        .max(result_count)
        .min(sources.len());
    let stub_overlay_set = set_java_stub_overlay(&classpath, &request.analysis.java_sources);
    let platform = Box::new(JvmLibraries::new(classpath.clone()));
    let analysis = compiler_analysis::analyze_source_inputs_prefix_with_features(
        &inputs,
        result_count,
        inferred_count,
        platform,
        &features,
    );
    // A second handle over the same `Rc<Classpath>`: the semantic platform was moved into the
    // analysis as a `Box<dyn SemanticPlatform>`, which cannot be re-borrowed as a `TargetRuntime`.
    let runtime = JvmLibraries::new(classpath.clone());
    let response = render_analyzed_dump(
        &analysis,
        &sources[request.target],
        request.target,
        &request.label,
        &request.cache_key,
        &request.cache_root,
        &runtime,
    );
    if stub_overlay_set {
        classpath.clear_stub_overlay();
    }
    response
}

/// Render the analyzed target file's document and store it under `cache_root`.
///
/// Everything the document needs — including driving the lowering that fills its IR section — is
/// the dump renderer's own business; the worker only picks the file out of its analysis and decides
/// where the result is written.
fn render_analyzed_dump(
    analysis: &compiler_analysis::SourceSetAnalysis,
    source: &str,
    target: usize,
    label: &str,
    cache_key: &str,
    cache_root: &Path,
    runtime: &JvmLibraries,
) -> Option<DumpResponse> {
    let file_analysis = analysis.files.get(target)?;
    let text = krusty::dump::render_file_dump_with_limit(
        &krusty::dump::FileDumpInput {
            label,
            source,
            file: &file_analysis.file,
            file_index: target,
            info: file_analysis.types.as_ref(),
            symbols: &analysis.symbols,
            runtime,
            diagnostics: &file_analysis.diagnostics,
        },
        crate::dump_cache::MAX_DUMP_BYTES,
    );

    let path = crate::dump_cache::store(cache_root, cache_key, &text).ok()?;
    Some(DumpResponse { path })
}

fn json_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, Cursor, Read};

    use super::*;
    use crate::analysis::MAX_SOURCE_SET_NAVIGATION_ENTRIES;

    struct DelayedEof {
        delay: Duration,
    }

    impl Read for DelayedEof {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(self.delay);
            Ok(0)
        }
    }

    struct MutatingReader {
        inner: Cursor<Vec<u8>>,
        mutation: Option<Box<dyn FnOnce()>>,
    }

    impl MutatingReader {
        fn mutate(&mut self) {
            if let Some(mutation) = self.mutation.take() {
                mutation();
            }
        }
    }

    impl Read for MutatingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.mutate();
            self.inner.read(buffer)
        }
    }

    impl BufRead for MutatingReader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            self.mutate();
            self.inner.fill_buf()
        }

        fn consume(&mut self, amount: usize) {
            self.inner.consume(amount);
        }
    }

    fn decode_worker_output(output: Vec<u8>) -> Vec<AnalysisResponse> {
        let mut output = Cursor::new(output);
        let ready = read_framed(&mut output, WORKER_READY.len())
            .unwrap()
            .expect("worker readiness message");
        assert_eq!(ready, WORKER_READY);
        let response = read_framed(&mut output, MAX_WORKER_MESSAGE_BYTES)
            .unwrap()
            .expect("worker analysis response");
        assert!(
            read_framed(&mut output, MAX_WORKER_MESSAGE_BYTES)
                .unwrap()
                .is_none(),
            "worker emitted an unexpected trailing frame"
        );
        serde_json::from_slice(&response).unwrap()
    }

    /// The dump path from a worker run that served exactly one dump request.
    fn read_dump_path(output: Vec<u8>) -> PathBuf {
        let mut output = Cursor::new(output);
        assert_eq!(
            read_framed(&mut output, WORKER_READY.len())
                .unwrap()
                .as_deref(),
            Some(WORKER_READY)
        );
        let body = read_framed(&mut output, MAX_WORKER_MESSAGE_BYTES)
            .unwrap()
            .expect("worker dump response");
        let response: Option<DumpResponse> = serde_json::from_slice(&body).unwrap();
        response.expect("dump produced").path
    }

    fn navigation_saturation_response(
        definitions: usize,
        type_definitions: usize,
        implementations: usize,
    ) -> AnalysisResponse {
        AnalysisResponse {
            diagnostics: Vec::new(),
            hover: HoverIndex::default(),
            completion: CompletionIndex::default(),
            signature_help: SignatureHelpIndex::default(),
            semantic_tokens: SemanticTokenIndex::default(),
            definitions: DefinitionIndex::wire_saturation_fixture(definitions),
            type_definitions: DefinitionIndex::wire_saturation_fixture(type_definitions),
            implementations: DefinitionIndex::wire_saturation_fixture(implementations),
            library_definitions: LibraryDefinitionIndex::default(),
            document_symbols: DocumentSymbolIndex::default(),
            workspace_symbols: WorkspaceSymbolIndex::default(),
            folding_ranges: FoldingRangeIndex::default(),
            implementation_relations: Vec::new(),
        }
    }

    #[test]
    fn implementation_relation_wire_order_is_stable_before_global_saturation() {
        let target = |file, lo| crate::compiler_analysis::DefinitionTarget {
            file,
            span: krusty::diag::Span::new(lo, lo + 1),
        };
        let base = target(0, 4);
        let first = target(1, 10);
        let second = target(2, 20);

        let forward = compact_implementation_relations([(base, first), (base, second)]);
        let reverse = compact_implementation_relations([(base, second), (base, first)]);

        assert_eq!(forward, reverse);
        assert_eq!(
            forward,
            [
                [base.file, base.span.lo, base.span.hi, 1, 10, 11],
                [base.file, base.span.lo, base.span.hi, 2, 20, 21],
            ]
        );
    }

    #[test]
    fn source_and_wire_buffers_are_bounded_before_worker_io() {
        assert!(source_set_fits([MAX_SOURCE_SET_BYTES]));
        assert!(!source_set_fits([MAX_SOURCE_SET_BYTES, 1]));
        let inputs = [SourceInput::kotlin("fun use() = 1")];
        assert_eq!(
            encode_request(&inputs, 1, 0, &LangFeatures::new(), &[], None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            encode_request(&inputs, 0, 2, &LangFeatures::new(), &[], None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            encode_request(
                &inputs,
                1,
                1,
                &LangFeatures::new(),
                &[String::from_utf8(vec![b'x'; MAX_SOURCE_SET_BYTES]).unwrap()],
                None,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );

        let mut output = BoundedVec::new(4);
        output.write_all(b"1234").unwrap();
        let error = output.write_all(b"5").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(output.bytes, b"1234");
    }

    #[test]
    fn worker_discards_analysis_when_classpath_snapshot_changes() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "krusty-worker-classpath-revision-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).expect("create classpath directory");

        let inputs = [SourceInput::kotlin("fun use() = 1")];
        let request = encode_request(&inputs, 1, 1, &LangFeatures::new(), &[], None).unwrap();
        let mut framed = Vec::new();
        write_framed(&mut framed, &request).unwrap();
        let generated = directory.join("generated");
        let mut reader = MutatingReader {
            inner: Cursor::new(framed),
            mutation: Some(Box::new(move || {
                std::fs::create_dir(&generated).expect("mutate classpath directory");
            })),
        };
        let mut output = Vec::new();

        run_analysis_worker(&mut reader, &mut output, vec![directory.clone()]).unwrap();

        let mut output = Cursor::new(output);
        assert_eq!(
            read_framed(&mut output, WORKER_READY.len())
                .unwrap()
                .as_deref(),
            Some(WORKER_READY)
        );
        assert!(
            read_framed(&mut output, MAX_WORKER_MESSAGE_BYTES)
                .unwrap()
                .is_none(),
            "stale worker must not produce an analysis response"
        );
        std::fs::remove_dir_all(directory).expect("remove classpath directory");
    }

    #[test]
    fn framed_worker_read_times_out_when_no_readiness_frame_arrives() {
        let receiver = framed_read_receiver(
            BufReader::new(DelayedEof {
                delay: Duration::from_millis(50),
            }),
            WORKER_READY.len(),
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let (_, result) = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn shared_navigation_budget_keeps_saturated_worker_response_below_frame_cap() {
        let baseline = serde_json::to_vec(&vec![navigation_saturation_response(
            MAX_SOURCE_SET_NAVIGATION_ENTRIES,
            0,
            0,
        )])
        .unwrap();
        assert!(baseline.len() < MAX_WORKER_MESSAGE_BYTES);

        let definition_entries = MAX_SOURCE_SET_NAVIGATION_ENTRIES / 3;
        let type_definition_entries = MAX_SOURCE_SET_NAVIGATION_ENTRIES / 3;
        let response = navigation_saturation_response(
            definition_entries,
            type_definition_entries,
            MAX_SOURCE_SET_NAVIGATION_ENTRIES - definition_entries - type_definition_entries,
        );
        assert_eq!(
            response.definitions.entry_count()
                + response.type_definitions.entry_count()
                + response.implementations.entry_count(),
            MAX_SOURCE_SET_NAVIGATION_ENTRIES
        );
        let encoded = serde_json::to_vec(&vec![response]).unwrap();
        assert!(encoded.len() < MAX_WORKER_MESSAGE_BYTES);
        assert!(
            encoded.len() <= baseline.len(),
            "type-definition and implementation entries must share the prior navigation frame"
        );
    }

    #[test]
    fn implementation_relations_share_navigation_and_response_wire_budgets() {
        let relations = vec![[u32::MAX; 6], [u32::MAX - 1; 6]];
        let mut navigation_saturated = vec![navigation_saturation_response(
            MAX_SOURCE_SET_NAVIGATION_ENTRIES - 1,
            0,
            0,
        )];

        retain_implementation_relations_for_response(
            &mut navigation_saturated,
            relations.clone(),
            MAX_SOURCE_SET_NAVIGATION_ENTRIES,
            MAX_WORKER_MESSAGE_BYTES,
        )
        .unwrap();

        assert_eq!(navigation_saturated[0].implementation_relations.len(), 1);
        assert!(encode_response(&navigation_saturated).is_ok());

        let mut wire_saturated = vec![navigation_saturation_response(0, 0, 0)];
        let baseline = encode_response(&wire_saturated).unwrap().len();
        let first_relation_wire = serde_json::to_vec(&relations[0]).unwrap().len();
        retain_implementation_relations_for_response(
            &mut wire_saturated,
            relations,
            usize::MAX,
            baseline + first_relation_wire,
        )
        .unwrap();

        assert_eq!(wire_saturated[0].implementation_relations.len(), 1);
        assert_eq!(
            encode_response(&wire_saturated).unwrap().len(),
            baseline + first_relation_wire
        );
    }

    #[test]
    fn launch_configuration_carries_a_classpath_larger_than_exec_arguments() {
        let classpath = (0..2_752)
            .map(|index| {
                PathBuf::from(format!(
                    "/workspace/out/production/module-{index:04}-{}",
                    "component".repeat(8)
                ))
            })
            .collect::<Vec<_>>();

        let encoded = encode_launch_configuration(&classpath).unwrap();

        assert!(
            encoded.len() > 128 * 1024,
            "the fixture must exceed the common Unix per-argument ceiling"
        );
        let decoded = serde_json::from_slice::<OwnedWorkerLaunchConfiguration>(&encoded).unwrap();
        assert_eq!(decoded.classpath, classpath);
    }

    #[test]
    fn worker_protocol_materializes_from_its_configured_classpath() {
        let directory =
            std::env::temp_dir().join(format!("krusty-worker-materialize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let classes = directory.join("widget.jar");
        let sources = directory.join("widget-sources.jar");
        let write_jar = |path: &Path, name: &str, content: &[u8]| {
            let file = std::fs::File::create(path).unwrap();
            let mut archive = zip::ZipWriter::new(file);
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(content).unwrap();
            archive.finish().unwrap();
        };
        write_jar(&classes, "sample/Widget.class", b"");
        write_jar(
            &sources,
            "sample/Widget.kt",
            b"package sample\nclass Widget { fun value() = 1 }\n",
        );
        let reference = LibraryRef {
            fqn: "sample/Widget".to_string(),
            member_name: "value".to_string(),
            member_desc: String::new(),
        };
        let request = encode_materialize_request(&reference, true).unwrap();
        let mut input = Vec::new();
        let configuration = encode_launch_configuration(std::slice::from_ref(&classes)).unwrap();
        write_framed(&mut input, &configuration).unwrap();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();

        run_configured_analysis_worker(&mut Cursor::new(input), &mut output).unwrap();

        let mut output = Cursor::new(output);
        assert_eq!(
            read_framed(&mut output, WORKER_READY.len())
                .unwrap()
                .as_deref(),
            Some(WORKER_READY)
        );
        let response = read_framed(&mut output, MAX_WORKER_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        let materialized: Option<MaterializeResponse> = serde_json::from_slice(&response).unwrap();
        let materialized = materialized.unwrap();
        assert_eq!(
            &materialized.text[materialized.lo as usize..materialized.hi as usize],
            "value"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn worker_protocol_dumps_a_target_file_to_a_markdown_path() {
        let root = std::env::temp_dir().join(format!("krusty-worker-dump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let request = serde_json::json!({
            "dump": {
                "analysis": {
                    "sources": ["fun box(): String = \"OK\"\n"],
                    "result_count": 1,
                },
                "target": 0,
                "label": "src/Main.kt",
                "cache_key": "file:///workspace/src/Main.kt",
                "cache_root": root,
            }
        });
        let mut input = Vec::new();
        write_framed(&mut input, &serde_json::to_vec(&request).unwrap()).unwrap();
        let mut output = Vec::new();

        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let path = read_dump_path(output);

        assert!(path.starts_with(&root), "{path:?}");
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("md"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# krusty dump — src/Main.kt"), "{text}");
        assert!(text.contains("## AST"), "{text}");
        assert!(text.contains("## Checker"), "{text}");
        assert!(text.contains("## IR"), "{text}");
        assert!(
            !text.contains("<file not checked>"),
            "the dumped file must carry its checker result: {text}"
        );
        assert!(
            !text.contains("not lowered:"),
            "the dumped file must carry lowered IR: {text}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn request_shapes_route_to_their_own_untagged_variants() {
        // The untagged enum resolves by field shape in declaration order, so adding a variant can
        // silently capture another shape's payload. Every shape is pinned here.
        let analyze = br#"{"sources":["fun answer(): Int = 42"],"result_count":1}"#;
        assert!(
            matches!(
                serde_json::from_slice::<OwnedWorkerRequest>(analyze).unwrap(),
                OwnedWorkerRequest::Analyze(_)
            ),
            "a bare analysis payload must still reach the analyze arm"
        );

        let materialize = serde_json::to_vec(&serde_json::json!({
            "materialize": {
                "reference": { "fqn": "sample/Widget", "member_name": "value", "member_desc": "" },
                "use_sources": true,
            }
        }))
        .unwrap();
        assert!(matches!(
            serde_json::from_slice::<OwnedWorkerRequest>(&materialize).unwrap(),
            OwnedWorkerRequest::Materialize { .. }
        ));

        let dump = serde_json::to_vec(&serde_json::json!({
            "dump": {
                "analysis": { "sources": ["fun answer(): Int = 42"], "result_count": 1 },
                "target": 0,
                "label": "src/Main.kt",
                "cache_key": "file:///workspace/src/Main.kt",
                "cache_root": "/tmp/krusty",
            }
        }))
        .unwrap();
        assert!(matches!(
            serde_json::from_slice::<OwnedWorkerRequest>(&dump).unwrap(),
            OwnedWorkerRequest::Dump { .. }
        ));
    }

    #[test]
    fn dump_requests_carry_the_module_configuration_to_the_worker() {
        let sources = vec![
            "fun use(w: p.Widget) {}".to_string(),
            "package p; public class Widget {}".to_string(),
        ];
        let java_sources = vec!["package p; public class Widget {}".to_string()];
        let classpath = vec![PathBuf::from("module.jar")];
        let language_arguments = vec!["-Xname-based-destructuring".to_string()];
        let mut session = LangFeatures::new();
        session.enable("SessionOnly");
        let target = DumpTarget {
            sources: &sources,
            source_kinds: &[SourceKind::Kotlin, SourceKind::Java],
            target: 0,
            label: "src/Main.kt",
            cache_key: "file:///workspace/src/Main.kt",
            cache_root: Path::new("/tmp/krusty"),
            result_count: 1,
            inferred_count: 2,
            java_sources: &java_sources,
            language_arguments: Some(&language_arguments),
            classpath: Some(&classpath),
        };

        let encoded = encode_dump_request(&target, &session).unwrap();

        let OwnedWorkerRequest::Dump { dump } =
            serde_json::from_slice::<OwnedWorkerRequest>(&encoded).unwrap()
        else {
            panic!("an encoded dump request must reach the dump arm");
        };
        assert_eq!(dump.target, 0);
        assert_eq!(dump.label, "src/Main.kt");
        assert_eq!(dump.cache_key, "file:///workspace/src/Main.kt");
        assert_eq!(dump.cache_root, PathBuf::from("/tmp/krusty"));
        assert_eq!(dump.analysis.sources, sources);
        assert_eq!(
            dump.analysis.source_kinds,
            vec![SourceKind::Kotlin.wire_code(), SourceKind::Java.wire_code()],
            "a dump must not reinterpret a Java document as Kotlin"
        );
        assert_eq!(dump.analysis.java_sources, java_sources);
        assert_eq!(dump.analysis.classpath.as_deref(), Some(&classpath[..]));
        assert_eq!(dump.analysis.result_count, 1);
        assert_eq!(dump.analysis.inferred_count, Some(2));
        assert_eq!(
            dump.analysis.language_features,
            vec![
                "ContextParameters".to_string(),
                "MultiDollarInterpolation".to_string(),
                "NameBasedDestructuring".to_string(),
            ],
            "the module's own language arguments must reach the worker, and must replace the \
             session features exactly as analyze_inputs_prefix_with_config does"
        );
    }

    #[test]
    fn dump_requests_fall_back_to_the_session_language_features() {
        let sources = vec!["fun answer(): Int = 42".to_string()];
        let mut session = LangFeatures::new();
        session.enable("ContextParameters");
        let target = DumpTarget {
            sources: &sources,
            source_kinds: &[SourceKind::Kotlin],
            target: 0,
            label: "src/Main.kt",
            cache_key: "file:///workspace/src/Main.kt",
            cache_root: Path::new("/tmp/krusty"),
            result_count: 1,
            inferred_count: 1,
            java_sources: &[],
            language_arguments: None,
            classpath: None,
        };

        let encoded = encode_dump_request(&target, &session).unwrap();

        let OwnedWorkerRequest::Dump { dump } =
            serde_json::from_slice::<OwnedWorkerRequest>(&encoded).unwrap()
        else {
            panic!("an encoded dump request must reach the dump arm");
        };
        assert_eq!(
            dump.analysis.language_features,
            vec![
                "ContextParameters".to_string(),
                "MultiDollarInterpolation".to_string(),
            ]
        );
        assert_eq!(dump.analysis.classpath, None);
    }

    #[test]
    fn worker_protocol_dumps_a_script_target_under_its_own_source_kind() {
        let root =
            std::env::temp_dir().join(format!("krusty-worker-dump-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // Top-level statements parse only under the script kind; ignoring the kinds would put parse
        // diagnostics in the Checker section for a file the editor reports as clean.
        let request = serde_json::json!({
            "dump": {
                "analysis": {
                    "sources": ["fun render(value: String): String = value\nrender(\"sample\")"],
                    "source_kinds": [SourceKind::KotlinScript.wire_code()],
                    "result_count": 1,
                },
                "target": 0,
                "label": "build.gradle.kts",
                "cache_key": "file:///workspace/build.gradle.kts",
                "cache_root": root,
            }
        });
        let mut input = Vec::new();
        write_framed(&mut input, &serde_json::to_vec(&request).unwrap()).unwrap();
        let mut output = Vec::new();

        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let text = std::fs::read_to_string(read_dump_path(output)).unwrap();
        assert!(text.contains("# krusty dump — build.gradle.kts"), "{text}");
        assert!(
            text.contains("no diagnostics"),
            "a script target must be parsed as a script: {text}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn worker_protocol_dumps_the_lowering_bail_reason() {
        let root =
            std::env::temp_dir().join(format!("krusty-worker-dump-bail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // A recursive `tailrec` member is checked fine but is not loop-transformed yet.
        let source = "class Counter {\n    tailrec fun count(value: Int): Int =\n        if (value == 0) 0 else count(value - 1)\n}\n\
                      fun box(): String = \"OK\"\n";
        let request = serde_json::json!({
            "dump": {
                "analysis": { "sources": [source], "result_count": 1 },
                "target": 0,
                "label": "src/Named.kt",
                "cache_key": "file:///workspace/src/Named.kt",
                "cache_root": root,
            }
        });
        let mut input = Vec::new();
        write_framed(&mut input, &serde_json::to_vec(&request).unwrap()).unwrap();
        let mut output = Vec::new();

        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let text = std::fs::read_to_string(read_dump_path(output)).unwrap();
        assert!(
            text.contains("not lowered: gate:tailrec-member"),
            "the bail reason must survive into the document: {text}"
        );
        assert!(
            text.contains("no diagnostics"),
            "a lowering bail is not a checker error: {text}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn worker_protocol_reports_no_dump_for_a_target_outside_the_source_set() {
        let root =
            std::env::temp_dir().join(format!("krusty-worker-dump-range-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let request = serde_json::json!({
            "dump": {
                "analysis": {
                    "sources": ["fun answer(): Int = 42\n"],
                    "result_count": 1,
                },
                "target": 4,
                "label": "src/Main.kt",
                "cache_key": "file:///workspace/src/Main.kt",
                "cache_root": root,
            }
        });
        let mut input = Vec::new();
        write_framed(&mut input, &serde_json::to_vec(&request).unwrap()).unwrap();
        let mut output = Vec::new();

        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let mut output = Cursor::new(output);
        assert_eq!(
            read_framed(&mut output, WORKER_READY.len())
                .unwrap()
                .as_deref(),
            Some(WORKER_READY)
        );
        let body = read_framed(&mut output, MAX_WORKER_MESSAGE_BYTES)
            .unwrap()
            .expect("worker dump response");
        let response: Option<DumpResponse> = serde_json::from_slice(&body).unwrap();

        assert!(response.is_none(), "out-of-range target must not dump");
        assert!(!root.exists(), "a rejected dump must not write anything");
    }

    #[test]
    fn worker_protocol_analyzes_a_cross_file_source_set() {
        let sources = [
            "package demo\nclass WorkerResult\nfun answer(): WorkerResult = WorkerResult()",
            "package demo\nfun use(): WorkerResult {\n  return answer()\n}",
            "package demo\n\
             interface WorkerContract {\n\
             \u{20}\u{20}fun run(): String\n\
             }\n\
             open class WorkerBase : WorkerContract {\n\
             \u{20}\u{20}override fun run(): String = \"base\"\n\
             }",
            "package demo\n\
             class WorkerLeaf : WorkerBase() {\n\
             \u{20}\u{20}override fun run(): String = \"leaf\"\n\
             }\n\
             fun use(value: WorkerContract): String = value.run()",
        ];
        let source_kinds = vec![0; sources.len()];
        let request = serde_json::to_vec(&AnalysisRequest {
            sources: &sources,
            source_kinds: &source_kinds,
            result_count: sources.len(),
            inferred_count: sources.len(),
            language_features: &[],
            java_sources: &[],
            classpath: None,
        })
        .unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();
        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let analyses = decode_worker_output(output);
        let analysis = analyses
            .into_iter()
            .last()
            .unwrap()
            .into_document_analysis();
        assert!(analysis.diagnostics.is_empty());
        assert!(analysis.hover.entry_count() > 0);
        assert!(analysis.completion.entry_count() > 0);
        assert!(analysis.signature_help.entry_count() > 0);
        assert!(analysis.semantic_tokens.entry_count() > 0);
        assert!(analysis.definitions.entry_count() > 0);
        assert!(analysis.type_definitions.entry_count() > 0);
        assert!(analysis.implementations.entry_count() > 0);
        assert!(analysis.document_symbols.entry_count() > 0);
        assert!(analysis.folding_ranges.entry_count() > 0);
    }

    #[test]
    fn worker_request_classpath_overrides_the_session_classpath() {
        let directory = std::env::temp_dir().join(format!(
            "krusty-worker-module-classpath-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let package = directory.join("hidden");
        std::fs::create_dir_all(&package).unwrap();
        let class =
            krusty::jvm::classfile::ClassWriter::new("hidden/Only", "java/lang/Object").finish();
        std::fs::write(package.join("Only.class"), class).unwrap();

        let sources = ["fun use(value: hidden.Only) {}"];
        let request = serde_json::to_vec(&AnalysisRequest {
            sources: &sources,
            source_kinds: &[0],
            result_count: 1,
            inferred_count: 1,
            language_features: &[],
            java_sources: &[],
            classpath: Some(&[]),
        })
        .unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();

        run_analysis_worker(
            &mut Cursor::new(input),
            &mut output,
            vec![directory.clone()],
        )
        .unwrap();

        let analyses = decode_worker_output(output);
        assert!(analyses[0]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Only")));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn worker_protocol_gives_java_documents_a_navigation_index() {
        let sources = [
            "package demo\nclass Greeter\n",
            "package demo;\n\nclass Use {\n    Greeter g;\n}\n",
        ];
        let source_kinds = vec![SourceKind::Kotlin.wire_code(), SourceKind::Java.wire_code()];
        let request = serde_json::to_vec(&AnalysisRequest {
            sources: &sources,
            source_kinds: &source_kinds,
            result_count: sources.len(),
            inferred_count: sources.len(),
            language_features: &[],
            java_sources: &[],
            classpath: None,
        })
        .unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();
        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let java = decode_worker_output(output)
            .into_iter()
            .nth(1)
            .unwrap()
            .into_document_analysis();
        assert!(java.diagnostics.is_empty());
        let targets = java.definitions.get(31).collect::<Vec<_>>();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].file, 0);
    }

    #[test]
    fn worker_protocol_points_kotlin_at_a_java_source_declaration() {
        let java = "package demo;\n\npublic record Gadget(int width, int height) {\n}\n";
        let sources = [java, "package demo\n\nfun make(): Gadget? = null\n"];
        let source_kinds = vec![SourceKind::Java.wire_code(), SourceKind::Kotlin.wire_code()];
        let java_sources = vec![java.to_string()];
        let request = serde_json::to_vec(&AnalysisRequest {
            sources: &sources,
            source_kinds: &source_kinds,
            result_count: sources.len(),
            inferred_count: sources.len(),
            language_features: &[],
            java_sources: &java_sources,
            classpath: None,
        })
        .unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();
        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let kotlin = decode_worker_output(output)
            .into_iter()
            .nth(1)
            .unwrap()
            .into_document_analysis();
        let targets = kotlin.definitions.get(26).collect::<Vec<_>>();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].file, 0);
        assert_eq!(
            &java[targets[0].span.lo as usize..targets[0].span.hi as usize],
            "Gadget"
        );
    }

    #[test]
    fn worker_protocol_accepts_an_older_request_without_an_inference_prefix() {
        let request = br#"{
            "sources":["fun answer(): Int = 42"],
            "source_kinds":[0],
            "result_count":1,
            "language_features":[]
        }"#;
        let mut input = Vec::new();
        write_framed(&mut input, request).unwrap();
        let mut output = Vec::new();

        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let analyses = decode_worker_output(output);
        assert!(analyses[0].diagnostics.is_empty());
    }

    #[test]
    fn worker_protocol_applies_project_language_features() {
        let sources = ["\
data class Entry(val first: String, val second: String)
fun combine(entries: Array<Entry>): String {
    var result = \"\"
    for ([left, right] in entries) {
        result += left + right
    }
    return result
}"];
        let source_kinds = vec![0; sources.len()];
        let request = serde_json::to_vec(&AnalysisRequest {
            sources: &sources,
            source_kinds: &source_kinds,
            result_count: sources.len(),
            inferred_count: sources.len(),
            language_features: &["NameBasedDestructuring"],
            java_sources: &[],
            classpath: None,
        })
        .unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();
        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let analyses = decode_worker_output(output);
        let diagnostics = &analyses[0].diagnostics;
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn worker_protocol_checks_kotlin_script_statements() {
        let sources = ["fun render(value: String): String = value\n\
             render(\"sample\")"];
        let request = serde_json::to_vec(&AnalysisRequest {
            sources: &sources,
            source_kinds: &[krusty::source::SourceKind::KotlinScript.wire_code()],
            result_count: 1,
            inferred_count: 1,
            language_features: &[],
            java_sources: &[],
            classpath: None,
        })
        .unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();

        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let analyses = decode_worker_output(output);
        assert!(analyses[0].diagnostics.is_empty());
    }

    #[test]
    fn worker_resolves_kotlin_reference_to_stubbed_java() {
        let kotlin = "fun use(w: p.Widget) {}";
        let request = format!(
            "{{\"sources\":[{}],\"source_kinds\":[0],\"result_count\":1,\
              \"java_sources\":[{}],\"language_features\":[]}}",
            serde_json::to_string(kotlin).unwrap(),
            serde_json::to_string("package p; public class Widget {}").unwrap(),
        );
        let mut input = Vec::new();
        write_framed(&mut input, request.as_bytes()).unwrap();
        let mut output = Vec::new();
        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let analyses = decode_worker_output(output);
        let diagnostics = &analyses[0].diagnostics;
        assert!(
            !diagnostics.iter().any(|d| d.message.contains("Widget")),
            "Widget resolved from stub: {diagnostics:?}"
        );
    }
}
