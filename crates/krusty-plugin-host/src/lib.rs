//! Runs real kotlinc compiler-plugin jars (Compose, Metro, power-assert, any third-party plugin) for
//! krusty, in a JVM that loads `kotlin-compiler-embeddable` at the build's exact Kotlin version.
//!
//! This crate is a process adapter, not part of the compiler: the compiler never downloads and never
//! starts a JVM. A build driver calls [`provision`] only once a compilation actually requests a
//! plugin jar that krusty does not implement natively, so a build without one never touches a JVM.
//!
//! [`provision`] finds or fetches what the host runs on:
//! - a JDK: `JAVA_HOME`, then `java` on `PATH`, then krusty's cache, then a pinned Temurin download;
//! - the compiler's runtime classpath: krusty's cache, then a verified copy from the Gradle or Maven
//!   local cache, then a download from a Maven repository.
//!
//! Every downloaded or copied file is checked against a SHA-256 pinned in this crate, and every
//! download is announced with what it is and why it is needed.
//!
//! [`Host::start`] then runs the host on what was provisioned: a JVM with krusty's adapter
//! (`adapter/Host.kt`, compiled once per Kotlin version by that version's compiler) in front of the
//! compiler, which loads plugin jars the way kotlinc's `-Xplugin` does.

mod adapter;
mod artifacts;
mod download;
mod environment;
mod host;
mod install;
mod jdk;
mod pins;
mod protocol;
mod provision;

pub use download::{Curl, Fetcher};
pub use environment::{Environment, Platform};
pub use host::{Host, HostError};
pub use pins::{ArtifactPin, JdkPin, Pins};
pub use protocol::LoadedPlugin;
pub use provision::{provision, HostRuntime, ProvisionError, Request, COMPILER_MIN_JAVA};
