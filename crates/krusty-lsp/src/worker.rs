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
use krusty::features::LangFeatures;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::source::{SourceInput, SourceKind};
use serde::{Deserialize, Serialize};

use crate::compiler_analysis::{
    self, CompletionSymbols, DefinitionSymbols, HighlightSymbols, SignatureHelpSymbols,
};
use crate::{
    finalize_navigation, read_framed, write_framed, AnalysisBudgets, CompletionIndex,
    DefinitionIndex, DocumentAnalysis, DocumentSymbolIndex, FoldingRangeIndex, HoverIndex,
    SemanticTokenIndex, SignatureHelpIndex, SourceSetIndexes,
};

pub const DEFAULT_ANALYSES_PER_WORKER: usize = 64;
const MAX_WORKER_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SOURCE_SET_BYTES: usize = 32 * 1024 * 1024;
const BASE_ANALYSIS_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ANALYSIS_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const ANALYSIS_MILLIS_PER_ADDITIONAL_SOURCE: u64 = 1_000;
const ANALYSIS_SECS_PER_SOURCE_MIB: u64 = 2;

#[derive(Serialize)]
struct AnalysisRequest<'a> {
    sources: &'a [&'a str],
    source_kinds: &'a [u8],
    result_count: usize,
    inferred_count: usize,
    language_features: &'a [&'a str],
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
}

#[derive(Debug, Deserialize, Serialize)]
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
    signature_help: SignatureHelpIndex,
    semantic_tokens: SemanticTokenIndex,
    definitions: DefinitionIndex,
    type_definitions: DefinitionIndex,
    implementations: DefinitionIndex,
    document_symbols: DocumentSymbolIndex,
    folding_ranges: FoldingRangeIndex,
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
            signature_help: analysis.signature_help,
            semantic_tokens: analysis.semantic_tokens,
            definitions: analysis.definitions,
            type_definitions: analysis.type_definitions,
            implementations: analysis.implementations,
            document_symbols: analysis.document_symbols,
            folding_ranges: analysis.folding_ranges,
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
            signature_help: self.signature_help,
            semantic_tokens: self.semantic_tokens,
            definitions: self.definitions,
            type_definitions: self.type_definitions,
            implementations: self.implementations,
            document_symbols: self.document_symbols,
            folding_ranges: self.folding_ranges,
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

fn encode_request(
    inputs: &[SourceInput<'_>],
    result_count: usize,
    inferred_count: usize,
    features: &LangFeatures,
) -> io::Result<Vec<u8>> {
    if !source_set_fits(inputs.iter().map(|source| source.text.len())) {
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

fn analysis_timeout(sources: &[SourceInput<'_>]) -> Duration {
    let additional_sources = sources.len().saturating_sub(1) as u64;
    let source_bytes = sources.iter().fold(0usize, |total, source| {
        total.saturating_add(source.text.len())
    });
    let source_mib = (source_bytes / (1024 * 1024)) as u64;
    BASE_ANALYSIS_TIMEOUT
        .saturating_add(Duration::from_millis(
            additional_sources.saturating_mul(ANALYSIS_MILLIS_PER_ADDITIONAL_SOURCE),
        ))
        .saturating_add(Duration::from_secs(
            source_mib.saturating_mul(ANALYSIS_SECS_PER_SOURCE_MIB),
        ))
        .min(MAX_ANALYSIS_TIMEOUT)
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

    fn read_response(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        let mut stdout = self
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("analysis worker stdout unavailable"))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let response = read_framed(&mut stdout, MAX_WORKER_MESSAGE_BYTES);
            let _ = sender.send((stdout, response));
        });
        match receiver.recv_timeout(timeout) {
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

    fn analyze(
        &mut self,
        inputs: &[SourceInput<'_>],
        result_count: usize,
        inferred_count: usize,
        language_features: &LangFeatures,
    ) -> io::Result<Vec<DocumentAnalysis>> {
        let request = encode_request(inputs, result_count, inferred_count, language_features)?;
        write_framed(&mut self.stdin, &request)?;
        drop(request);
        let response = self.read_response(analysis_timeout(inputs))?;
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
    language_features: LangFeatures,
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
            language_features: LangFeatures::new(),
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

    /// Point the worker at a new classpath and JDK, restarting it so the change takes effect.
    ///
    /// The worker interns compiler-global types for its whole lifetime, so this cannot be applied in
    /// place — the process is replaced, which is exactly the bounded-restart path the supervisor
    /// already relies on. The classpath and JDK launch arguments are rebuilt from the parameters;
    /// unrelated arguments keep their order. When nothing would change, the worker is left running.
    pub fn reconfigure(
        &mut self,
        classpath: &[PathBuf],
        jdk_home: Option<&Path>,
        no_jdk: bool,
    ) -> io::Result<()> {
        let arguments = replace_launch_arguments(&self.arguments, classpath, jdk_home, no_jdk);
        if arguments == self.arguments {
            return Ok(());
        }
        let replacement = WorkerProcess::spawn(&self.executable, &arguments)?;
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
        self.process = replacement;
        self.arguments = arguments;
        self.analyses = 0;
        Ok(())
    }

    pub fn analyze(&mut self, sources: &[&str]) -> io::Result<Vec<DocumentAnalysis>> {
        let inputs = sources
            .iter()
            .map(|source| SourceInput::kotlin(source))
            .collect::<Vec<_>>();
        self.analyze_inputs_prefix(&inputs, sources.len(), sources.len())
    }

    pub fn analyze_inputs_prefix(
        &mut self,
        inputs: &[SourceInput<'_>],
        result_count: usize,
        inferred_count: usize,
    ) -> io::Result<Vec<DocumentAnalysis>> {
        if self.analyses >= self.max_analyses {
            self.restart()?;
        }
        match self.process.analyze(
            inputs,
            result_count,
            inferred_count,
            &self.language_features,
        ) {
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
                let analysis = self.process.analyze(
                    inputs,
                    result_count,
                    inferred_count,
                    &self.language_features,
                )?;
                self.analyses += 1;
                Ok(analysis)
            }
        }
    }

    pub fn set_language_features(&mut self, features: LangFeatures) {
        self.language_features = features;
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
        let mut language_features = LangFeatures::new();
        for feature in &request.language_features {
            language_features.enable(feature);
        }
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
        let definition_symbols = DefinitionSymbols::from_source_set(
            &sources,
            &source_set.files,
            &source_set.symbols,
            crate::analysis::MAX_SOURCE_SET_NAVIGATION_ENTRIES,
        );
        let completion_symbols = CompletionSymbols::from_source_set(&source_set.files);
        let signature_help_symbols =
            SignatureHelpSymbols::from_source_set(&sources, &source_set.files, &source_set.symbols);
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
        let analyses = finalize_navigation(pending, &mut budgets)
            .into_iter()
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

/// Rebuild a worker argument vector with a fresh classpath and JDK: drop every existing
/// `-cp`/`-classpath` pair, `-jdk-home` pair, and `-no-jdk` flag, then re-add them from the
/// parameters. The worker resolves JDK modules from `-jdk-home` (or `JAVA_HOME`) itself, so the
/// classpath here carries project entries only. Unrelated arguments keep their order.
fn replace_launch_arguments(
    arguments: &[String],
    classpath: &[PathBuf],
    jdk_home: Option<&Path>,
    no_jdk: bool,
) -> Vec<String> {
    let mut rebuilt = Vec::with_capacity(arguments.len());
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-cp" | "-classpath" | "-class-path" | "-jdk-home" => index += 2,
            "-no-jdk" => index += 1,
            other => {
                rebuilt.push(other.to_string());
                index += 1;
            }
        }
    }
    if no_jdk {
        rebuilt.push("-no-jdk".to_string());
    } else if let Some(jdk_home) = jdk_home {
        rebuilt.push("-jdk-home".to_string());
        rebuilt.push(jdk_home.to_string_lossy().into_owned());
    }
    if !classpath.is_empty() {
        rebuilt.push("-cp".to_string());
        rebuilt.push(join_classpath(classpath));
    }
    rebuilt
}

fn join_classpath(classpath: &[PathBuf]) -> String {
    std::env::join_paths(classpath)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            classpath
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join(if cfg!(windows) { ";" } else { ":" })
        })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::analysis::MAX_SOURCE_SET_NAVIGATION_ENTRIES;

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
            document_symbols: DocumentSymbolIndex::default(),
            folding_ranges: FoldingRangeIndex::default(),
        }
    }

    #[test]
    fn source_and_wire_buffers_are_bounded_before_worker_io() {
        assert!(source_set_fits([MAX_SOURCE_SET_BYTES]));
        assert!(!source_set_fits([MAX_SOURCE_SET_BYTES, 1]));
        let inputs = [SourceInput::kotlin("fun use() = 1")];
        assert_eq!(
            encode_request(&inputs, 1, 0, &LangFeatures::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            encode_request(&inputs, 0, 2, &LangFeatures::new())
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
    fn analysis_deadline_scales_with_the_complete_source_set_and_stays_bounded() {
        assert_eq!(
            analysis_timeout(&[SourceInput::kotlin("fun one() = 1")]),
            BASE_ANALYSIS_TIMEOUT
        );

        let module = vec![SourceInput::kotlin(""); 96];
        assert_eq!(
            analysis_timeout(&module),
            BASE_ANALYSIS_TIMEOUT + Duration::from_secs(95)
        );
        let one_mib = "x".repeat(1024 * 1024);
        assert_eq!(
            analysis_timeout(&[SourceInput::kotlin(&one_mib)]),
            BASE_ANALYSIS_TIMEOUT + Duration::from_secs(2)
        );

        let maximum = vec![SourceInput::kotlin(""); 1_000];
        assert_eq!(analysis_timeout(&maximum), MAX_ANALYSIS_TIMEOUT);
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
    fn reconfigure_replaces_classpath_and_jdk_while_keeping_other_arguments() {
        let arguments = vec![
            "--stdio".to_string(),
            "-cp".to_string(),
            "old.jar".to_string(),
            "-jdk-home".to_string(),
            "/old-jdk".to_string(),
        ];
        let rebuilt = replace_launch_arguments(
            &arguments,
            &[PathBuf::from("a.jar"), PathBuf::from("classes")],
            Some(Path::new("/jdk21")),
            false,
        );
        let expected_cp = join_classpath(&[PathBuf::from("a.jar"), PathBuf::from("classes")]);
        assert_eq!(
            rebuilt,
            vec![
                "--stdio".to_string(),
                "-jdk-home".to_string(),
                "/jdk21".to_string(),
                "-cp".to_string(),
                expected_cp,
            ]
        );
    }

    #[test]
    fn reconfigure_honors_no_jdk_and_an_empty_classpath() {
        let arguments = vec![
            "-jdk-home".to_string(),
            "/old-jdk".to_string(),
            "-classpath".to_string(),
            "old.jar".to_string(),
        ];
        assert_eq!(
            replace_launch_arguments(&arguments, &[], None, true),
            vec!["-no-jdk".to_string()]
        );
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
        })
        .unwrap();
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

        let response = read_framed(&mut Cursor::new(output), MAX_WORKER_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        let analyses = serde_json::from_slice::<Vec<AnalysisResponse>>(&response).unwrap();
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
        })
        .unwrap();
        let mut input = Vec::new();
        write_framed(&mut input, &request).unwrap();
        let mut output = Vec::new();
        run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

        let response = read_framed(&mut Cursor::new(output), MAX_WORKER_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        let analyses = serde_json::from_slice::<Vec<AnalysisResponse>>(&response).unwrap();
        let diagnostics = &analyses[0].diagnostics;
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}
