//! The host process: a JVM running the adapter on the provisioned compiler, which krusty drives over
//! its stdin and stdout. Its stderr is the user's, so what a plugin prints reaches them.

use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::adapter;
use crate::environment::Environment;
use crate::protocol::{self, LoadedPlugin, Reply, Request, PROTOCOL};
use crate::provision::HostRuntime;
use crate::session::{CallableIdentity, SessionRequest};

#[derive(Debug, PartialEq, Eq)]
pub enum HostError {
    /// The adapter did not compile.
    Build {
        detail: String,
    },
    Io {
        path: PathBuf,
        detail: String,
    },
    /// The JVM could not be started.
    Start {
        java: PathBuf,
        detail: String,
    },
    /// The host exited, or closed its end, before answering.
    Exited {
        status: String,
    },
    /// The host's output is not the protocol.
    Protocol {
        detail: String,
    },
    /// The host speaks another protocol version than this krusty.
    ProtocolVersion {
        host: u32,
    },
    /// The compiler the host loaded is not the build's exact Kotlin version.
    KotlinVersion {
        host: String,
        build: String,
    },
    /// The plugins refused the request, with kotlinc's own message: an unknown option, a missing
    /// required one, or a registrar that failed.
    Plugin {
        message: String,
    },
}

impl HostError {
    pub(crate) fn io(path: &Path, error: std::io::Error) -> Self {
        HostError::Io {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Build { detail } => {
                write!(f, "could not compile krusty's plugin host: {detail}")
            }
            HostError::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
            HostError::Start { java, detail } => write!(
                f,
                "could not start the plugin host with {}: {detail}",
                java.display()
            ),
            HostError::Exited { status } => write!(
                f,
                "the plugin host stopped before answering ({status}); its output above says why"
            ),
            HostError::Protocol { detail } => {
                write!(f, "the plugin host sent a malformed message: {detail}")
            }
            HostError::ProtocolVersion { host } => write!(
                f,
                "the plugin host speaks protocol {host}, but this krusty speaks {PROTOCOL}"
            ),
            HostError::KotlinVersion { host, build } => write!(
                f,
                "the plugin host loaded kotlinc {host}, but the build's Kotlin version is {build}"
            ),
            HostError::Plugin { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for HostError {}

/// How long a host may take to exit once asked to, or once its connection is lost, before it is
/// killed. A plugin can start threads that keep a JVM alive, and none of them may hang krusty.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// A running host. Dropping it stops the process: it is asked to exit, and killed when it has not
/// within [`EXIT_GRACE`].
pub struct Host {
    child: Child,
    /// `None` once closed, which tells the host to exit as a shutdown request does.
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: BufReader<ChildStdout>,
    grace: Duration,
}

impl Host {
    /// Starts a host on `runtime`, compiling the adapter first if the cache has none for it, and
    /// checks that it loaded the build's exact Kotlin version.
    pub fn start(
        runtime: &HostRuntime,
        env: &Environment,
        report: &mut dyn Write,
    ) -> Result<Host, HostError> {
        let jar = adapter::jar(runtime, env, report)?;
        let classpath = std::iter::once(&jar).chain(&runtime.compiler_classpath);
        let classpath = std::env::join_paths(classpath).map_err(|error| HostError::Start {
            java: runtime.java.clone(),
            detail: format!("the classpath cannot be passed to java: {error}"),
        })?;
        let mut command = runtime.java_command();
        command.arg("-cp").arg(classpath).arg(adapter::MAIN_CLASS);
        Host::spawn(command, &runtime.kotlin_version, EXIT_GRACE)
    }

    /// Runs `command` as a host and reads its greeting.
    fn spawn(
        mut command: Command,
        kotlin_version: &str,
        grace: Duration,
    ) -> Result<Host, HostError> {
        let program = PathBuf::from(command.get_program());
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| HostError::Start {
                java: program,
                detail: error.to_string(),
            })?;
        let stdin = Some(BufWriter::new(child.stdin.take().expect("a piped stdin")));
        let stdout = BufReader::new(child.stdout.take().expect("a piped stdout"));
        let mut host = Host {
            child,
            stdin,
            stdout,
            grace,
        };
        let hello = protocol::read_hello(&mut host.stdout).map_err(|error| host.failure(error))?;
        if hello.protocol != PROTOCOL {
            return Err(HostError::ProtocolVersion {
                host: hello.protocol,
            });
        }
        if hello.kotlin_version != kotlin_version {
            return Err(HostError::KotlinVersion {
                host: hello.kotlin_version,
                build: kotlin_version.to_string(),
            });
        }
        Ok(host)
    }

    /// Loads a plugin classpath as kotlinc's `-Xplugin` does, with `options` the `-P` values
    /// (`plugin:<id>:<key>=<value>`), and returns each registrar it declares, in the order kotlinc's
    /// service loader finds them, with the extensions it added. The registrars share one extension
    /// storage, as in kotlinc. An entry that is neither a directory nor a jar is refused, and so is a
    /// classpath declaring a legacy `ComponentRegistrar`, rather than loading as no plugin. What was
    /// loaded before is unloaded first; a failed load leaves nothing loaded.
    pub fn load(
        &mut self,
        entries: &[String],
        options: &[String],
    ) -> Result<Vec<LoadedPlugin>, HostError> {
        let request = Request::Load { entries, options };
        match self.exchange(&request)? {
            Reply::Loaded(plugins) => Ok(plugins),
            Reply::Failed(message) => Err(HostError::Plugin { message }),
            _ => Err(unexpected("load")),
        }
    }

    /// Ends the loaded plugins' lifetime: the resources their registrars registered are disposed as
    /// kotlinc disposes them, and their class loader is closed. Stopping the host does the same.
    pub fn unload(&mut self) -> Result<(), HostError> {
        match self.exchange(&Request::Unload)? {
            Reply::Unloaded => Ok(()),
            Reply::Failed(message) => Err(HostError::Plugin { message }),
            _ => Err(unexpected("unload")),
        }
    }

    /// Opens the compilation's session: kotlinc's JVM frontend and fir2ir run over `request`'s
    /// classpath with no sources, as kotlinc starts a compilation, so IR plugins find the build's
    /// declarations. The session open before is closed first. kotlinc's errors refuse the session,
    /// in kotlinc's words; its warnings go to stderr.
    pub fn open_session(&mut self, request: &SessionRequest) -> Result<(), HostError> {
        match self.exchange(&Request::OpenSession(request))? {
            Reply::SessionOpened => Ok(()),
            Reply::Failed(message) => Err(HostError::Plugin { message }),
            _ => Err(unexpected("open-session")),
        }
    }

    /// Closes the open session, if any, disposing what kotlinc holds for it. Stopping the host does
    /// the same.
    pub fn close_session(&mut self) -> Result<(), HostError> {
        match self.exchange(&Request::CloseSession)? {
            Reply::SessionClosed => Ok(()),
            Reply::Failed(message) => Err(HostError::Plugin { message }),
            _ => Err(unexpected("close-session")),
        }
    }

    /// Finds each of `identities` on the session's classpath through the plugin context IR plugins
    /// use, and returns it rendered as kotlinc renders IR. An identity that names no declaration, or
    /// more than one, fails the request by name.
    pub fn resolve(&mut self, identities: &[CallableIdentity]) -> Result<Vec<String>, HostError> {
        match self.exchange(&Request::Resolve(identities))? {
            Reply::Resolved(declarations) => Ok(declarations),
            Reply::Failed(message) => Err(HostError::Plugin { message }),
            _ => Err(unexpected("resolve")),
        }
    }

    /// The host process's id.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    fn exchange(&mut self, request: &Request) -> Result<Reply, HostError> {
        let stdin = self.stdin.as_mut().expect("open until the host is dropped");
        protocol::write_request(stdin, request).map_err(|error| self.failure(error))?;
        protocol::read_reply(&mut self.stdout).map_err(|error| self.failure(error))
    }

    /// The error for a failed read or write: the host's exit when it is gone, else the bad message.
    fn failure(&mut self, error: std::io::Error) -> HostError {
        match error.kind() {
            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe => {
                HostError::Exited {
                    status: self.reap(),
                }
            }
            _ => HostError::Protocol {
                detail: error.to_string(),
            },
        }
    }
}

impl Host {
    /// Waits up to the grace interval for the host to exit, then kills it, and always reaps it.
    /// Says how it ended.
    fn reap(&mut self) -> String {
        let deadline = Instant::now() + self.grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.to_string(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => break,
                Err(error) => return format!("its exit status is unknown: {error}"),
            }
        }
        let _ = self.child.kill();
        match self.child.wait() {
            Ok(_) => format!(
                "it did not exit within {} s and was stopped",
                self.grace.as_secs_f64()
            ),
            Err(error) => format!("it did not exit and could not be reaped: {error}"),
        }
    }
}

fn unexpected(request: &str) -> HostError {
    HostError::Protocol {
        detail: format!("the reply does not answer a {request} request"),
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // A host that cannot take the request is ended the same way, after the same grace.
        if let Some(mut stdin) = self.stdin.take() {
            let _ = protocol::write_request(&mut stdin, &Request::Shutdown);
        }
        self.reap();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The host's greeting for protocol 2 and kotlinc 2.4.20, as `printf` octal escapes.
    const HELLO: &str = r"\000\000\000\017\000\000\000\000\002\000\000\000\0062.4.20";
    const GRACE: Duration = Duration::from_millis(300);

    /// A host that writes its pid to `pid_file`, greets, then runs `then` in the same process.
    fn fixture(then: &str, pid_file: &Path) -> Host {
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "echo $$ > '{}'; printf '{HELLO}'; {then}",
            pid_file.display()
        ));
        Host::spawn(command, "2.4.20", GRACE).unwrap()
    }

    fn pid_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("krusty-host-{name}-{}", std::process::id()))
    }

    /// Whether the process `pid` still exists, zombies included, which a reaped child is not.
    fn exists(pid_file: &Path) -> bool {
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let alive = Path::new("/proc").join(pid.trim()).exists()
            || Command::new("kill")
                .args(["-0", pid.trim()])
                .status()
                .unwrap()
                .success();
        let _ = std::fs::remove_file(pid_file);
        alive
    }

    #[test]
    fn a_host_that_ignores_shutdown_is_killed_after_the_grace_interval() {
        let pids = pid_file("ignores-shutdown");
        // Never reads its stdin and never exits on its own, like a JVM a plugin thread keeps alive.
        let host = fixture("exec sleep 600", &pids);
        let started = Instant::now();
        drop(host);
        let took = started.elapsed();
        assert!(took >= GRACE && took < GRACE * 4, "{took:?}");
        assert!(!exists(&pids));
    }

    #[test]
    fn a_host_that_drops_its_connection_but_stays_alive_is_stopped_and_reported() {
        let pids = pid_file("drops-connection");
        let mut host = fixture("exec sleep 600 <&- >&-", &pids);
        let started = Instant::now();
        let error = host.load(&[], &[]).unwrap_err();
        let took = started.elapsed();
        assert_eq!(
            error,
            HostError::Exited {
                status: "it did not exit within 0.3 s and was stopped".into()
            }
        );
        assert!(took >= GRACE && took < GRACE * 4, "{took:?}");
        drop(host);
        assert!(!exists(&pids));
    }

    #[test]
    fn a_host_that_exits_on_shutdown_is_not_waited_on() {
        let pids = pid_file("exits");
        let host = fixture("exec cat > /dev/null", &pids);
        let started = Instant::now();
        drop(host);
        assert!(started.elapsed() < GRACE, "{:?}", started.elapsed());
        assert!(!exists(&pids));
    }
}
