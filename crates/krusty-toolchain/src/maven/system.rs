//! The host facts a POM may consult: the Java system properties the toolchain's JVM would report,
//! environment variables (`env.NAME`), and the operating system profile activation tests.

/// `os.name` as the JVM reports it.
fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Mac OS X",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        other => other,
    }
}

/// `os.arch` as the JVM reports it.
fn os_arch() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "x86_64",
        (_, "x86_64") => "amd64",
        (_, "x86") => "x86",
        (_, arch) => arch,
    }
}

/// `os.version`: the kernel release on Linux, unknown elsewhere.
fn os_version() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_string())
        .unwrap_or_default()
}

/// A Java system property the toolchain's JVM defines.
fn system_property(name: &str) -> Option<String> {
    let windows = cfg!(windows);
    let value = match name {
        "os.name" => os_name().to_string(),
        "os.arch" => os_arch().to_string(),
        "os.version" => os_version(),
        "file.separator" => if windows { "\\" } else { "/" }.to_string(),
        "path.separator" => if windows { ";" } else { ":" }.to_string(),
        "line.separator" => if windows { "\r\n" } else { "\n" }.to_string(),
        "user.home" => std::env::var(if windows { "USERPROFILE" } else { "HOME" }).ok()?,
        "user.dir" => std::env::current_dir().ok()?.display().to_string(),
        _ => return None,
    };
    Some(value)
}

/// A property a POM names that it does not declare itself
/// (`systemPropertyOrEnvironmentVariable`): `env.NAME` is an environment variable, anything else
/// a system property.
pub fn property(name: &str) -> Option<String> {
    match name.strip_prefix("env.") {
        Some(variable) if cfg!(windows) => std::env::var(variable.to_uppercase()).ok(),
        Some(variable) => std::env::var(variable).ok(),
        None => system_property(name),
    }
}

/// Whether the host is in the operating system family `family` (plexus `Os.isFamily`).
fn is_family(family: &str) -> bool {
    let family = family.to_lowercase();
    let os = std::env::consts::OS;
    match family.as_str() {
        "windows" | "dos" => os == "windows",
        "mac" => os == "macos",
        "unix" => os != "windows",
        _ => false,
    }
}

/// Which part of the operating system an `<os>` activation tests.
pub enum OsParameter {
    Name,
    Family,
    Arch,
    Version,
}

impl OsParameter {
    /// Whether the host matches `expected` (`OsActivationParameter.matches`): absent always
    /// matches, `!value` negates, `regex:` applies to the version only.
    pub fn matches(&self, expected: Option<&str>) -> bool {
        let Some(expected) = expected else {
            return true;
        };
        let actual = match self {
            OsParameter::Name => os_name().to_lowercase(),
            OsParameter::Arch => os_arch().to_lowercase(),
            OsParameter::Version => os_version().to_lowercase(),
            OsParameter::Family => String::new(),
        };
        if let (OsParameter::Version, Some(pattern)) = (self, expected.strip_prefix("regex:")) {
            // Without a regex engine the version can only match itself verbatim.
            return actual == pattern;
        }
        let (negated, expected) = match expected.strip_prefix('!') {
            Some(expected) => (true, expected),
            None => (false, expected),
        };
        let matches = match self {
            OsParameter::Family => is_family(expected),
            _ => actual.eq_ignore_ascii_case(expected),
        };
        matches != negated
    }
}
