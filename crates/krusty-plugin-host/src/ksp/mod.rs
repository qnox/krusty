//! Runs a module's KSP processors for krusty in a JVM that holds only KSP's API, the processors and
//! krusty's adapter, which serves `Resolver` and the `KS*` nodes from the declarations krusty sends
//! each round ([`KspRound`]). No compiler runs in that JVM: krusty analyzed the sources, and krusty
//! decides when another round runs.
//!
//! As for the plugin host, the compiler never starts this JVM. A build driver provisions it
//! ([`crate::provision_ksp`]) only for a module that names processors.

mod adapter;
mod protocol;

use std::io::Write;
use std::path::PathBuf;

use krusty_plugin_ir::KspRound;

use crate::host::HostError;
use crate::process::{HostProcess, EXIT_GRACE};
use crate::provision::KspRuntime;
use crate::Environment;

use protocol::{Reply, Request, PROTOCOL};

/// Where the processors' generated files go, by kind, as KSP's code generator places them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspOutputs {
    pub kotlin: PathBuf,
    pub java: PathBuf,
    pub classes: PathBuf,
    pub resources: PathBuf,
}

/// What loading a module's processors takes: their classpath and options, and what KSP tells them
/// about the compilation.
pub struct KspLoad<'a> {
    pub processor_classpath: &'a [PathBuf],
    /// `key=value` processor options, in the order the build gives them.
    pub options: &'a [(String, String)],
    pub module_name: &'a str,
    pub outputs: &'a KspOutputs,
    /// `SymbolProcessorEnvironment.kotlinVersion`, the module's language version.
    pub language_version: &'a str,
    pub api_version: &'a str,
    pub compiler_version: &'a str,
    pub ksp_version: &'a str,
    pub jvm_target: &'a str,
    pub jvm_default_mode: &'a str,
    /// KSP's logging level: 0 logging, 1 info, 3 warn, 5 error. Messages below it are dropped.
    pub log_level: u32,
    pub warnings_as_errors: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedKind {
    Kotlin,
    Java,
    Class,
    Resource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspGenerated {
    pub kind: GeneratedKind,
    pub path: PathBuf,
}

/// What one round, or the processors' finish, produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspOutcome {
    /// The files created, in creation order.
    pub generated: Vec<KspGenerated>,
    /// What the processors logged, each line as KSP prints it (`w: [ksp] …`).
    pub log: Vec<String>,
    /// Whether a processor logged an error, which ends processing.
    pub has_error: bool,
}

/// A running KSP host. Dropping it stops the process.
pub struct KspHost {
    process: HostProcess,
}

impl KspHost {
    /// Starts a host on `runtime`, compiling the adapter first if the cache has none for it.
    pub fn start(
        runtime: &KspRuntime,
        env: &Environment,
        report: &mut dyn Write,
    ) -> Result<KspHost, HostError> {
        let jar = adapter::jar(runtime, env, report)?;
        let classpath = std::iter::once(&jar).chain(&runtime.api_classpath);
        let classpath = std::env::join_paths(classpath).map_err(|error| HostError::Start {
            java: runtime.java.clone(),
            detail: format!("the classpath cannot be passed to java: {error}"),
        })?;
        let mut command = runtime.java_command();
        command.arg("-cp").arg(classpath).arg(adapter::MAIN_CLASS);
        let mut process = HostProcess::spawn(command, EXIT_GRACE, protocol::SHUTDOWN)?;
        let version =
            protocol::read_hello(process.stdout()).map_err(|error| process.failure(error))?;
        if version != PROTOCOL {
            return Err(HostError::ProtocolVersion { host: version });
        }
        Ok(KspHost { process })
    }

    /// Loads the processors on `load`'s classpath, as KSP finds them through its service loader,
    /// and creates each one with the environment `load` describes. Returns their providers' classes.
    pub fn load(&mut self, load: &KspLoad) -> Result<Vec<String>, HostError> {
        match self.exchange(&Request::Load(load))? {
            Reply::Loaded(providers) => Ok(providers),
            reply => Err(refused(reply, "load")),
        }
    }

    /// Runs every processor's `process` once over `round`. A file of `round` the previous rounds did
    /// not have is one of `Resolver.getNewFiles`.
    pub fn round(&mut self, round: &KspRound) -> Result<KspOutcome, HostError> {
        match self.exchange(&Request::Round(round))? {
            Reply::RoundDone(outcome) => Ok(outcome),
            reply => Err(refused(reply, "round")),
        }
    }

    /// Ends processing: every processor's `finish`, or `onError` when one logged an error.
    pub fn finish(&mut self) -> Result<KspOutcome, HostError> {
        match self.exchange(&Request::Finish)? {
            Reply::Finished(outcome) => Ok(outcome),
            reply => Err(refused(reply, "finish")),
        }
    }

    /// The host process's id.
    pub fn id(&self) -> u32 {
        self.process.id()
    }

    fn exchange(&mut self, request: &Request) -> Result<Reply, HostError> {
        protocol::write_request(self.process.stdin(), request)
            .map_err(|error| self.process.failure(error))?;
        protocol::read_reply(self.process.stdout()).map_err(|error| self.process.failure(error))
    }
}

fn refused(reply: Reply, request: &str) -> HostError {
    match reply {
        Reply::Unsupported(member) => HostError::Unsupported { member },
        Reply::Failed(message) => HostError::Plugin { message },
        _ => HostError::Protocol {
            detail: format!("the reply does not answer a {request} request"),
        },
    }
}
