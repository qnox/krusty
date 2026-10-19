//! A compilation's session in the host: kotlinc's JVM frontend and fir2ir run over the build's
//! classpath, which is what IR plugins look declarations up in.

use std::path::PathBuf;

/// What a session is opened over: the build's exact classpath and JDK, and the settings that decide
/// how kotlinc reads them, spelled as kotlinc's command line spells them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRequest {
    /// Every classpath entry, the standard library included: the host adds none of its own.
    pub classpath: Vec<PathBuf>,
    pub jdk_home: PathBuf,
    pub module_name: String,
    /// `-language-version`, such as `2.4`.
    pub language_version: String,
    /// `-api-version`, such as `2.4`.
    pub api_version: String,
    /// `-jvm-target`, such as `1.8` or `21`.
    pub jvm_target: String,
}
