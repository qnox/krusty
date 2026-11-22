//! `krusty kotlinc-native`: Kotlin/Native's `kotlinc-native` compiler, taking its options.
//!
//! It produces a program. A library is not written yet, because krusty's KLIBs do not carry
//! ordinary function bodies: a first-party library's sources are compiled into the program instead.
//!
//! The options are spelled as Kotlin/Native 2.4.20's `kotlinc-native -help` lists them, and an
//! option's value follows it (`-o app`) or is joined to it by `=` (`-produce=library`). An option
//! krusty cannot honor is refused by name rather than ignored, since the result would silently
//! differ.
//!
//! The compiler arguments every Kotlin compiler shares (the language settings, opt-ins and a
//! multiplatform fragment structure) are read with the sources by the same reader `krusty` uses
//! for kotlinc's, so a source set means one thing whichever target compiles it.

use std::path::PathBuf;

use krusty::kotlin_version::KotlincDefects;
use krusty_native::{NativeCompileOptions, NativeSource, NativeTarget};

use crate::cli::LanguageArgument;

/// The shared compiler arguments other than the language settings that a native compilation
/// honors: the multiplatform fragment structure and the opt-ins.
const SHARED_ARGUMENTS: &[&str] = &[
    "-Xfragments",
    "-Xfragment-sources",
    "-Xfragment-refines",
    "-Xexpect-actual-classes",
    "-opt-in",
    "-Xopt-in",
];

pub const HELP: &str = "\
Usage: krusty kotlinc-native <options> <source files>
Options:
    -library, -l <path> -> link with the library (a .klib or an unpacked library directory)
    -produce, -p {program} -> what to produce [program]
    -output, -o <name> -> the program to write; .kexe is appended [program]
    -target <target> -> native target to compile to (linux_x64, linux_arm64) [host]
    -module-name <name> -> the program's module name [the output's file name]
    -no-default-libs -> don't link the libraries from dist/klib automatically
    -Xfix-kotlinc-defects -> fix kotlinc's defects instead of reproducing them (krusty's own)
    -help, -h -> usage info
The language settings, -opt-in, -Xmulti-platform and the -Xfragments structure are read as
kotlinc reads them.
";

/// What a `kotlinc-native` command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Help,
    /// A program to compile, and whether to reproduce kotlinc's defects in it.
    Compile(Box<NativeCompileOptions>, KotlincDefects),
}

/// Read the arguments that follow `krusty kotlinc-native`.
pub fn parse(arguments: &[String]) -> Result<Invocation, String> {
    // The shared compiler arguments and the sources, in command-line order, for kotlinc's reader.
    let mut shared = Vec::new();
    let mut has_sources = false;
    let mut libraries = Vec::new();
    let mut output = None;
    let mut target = None;
    let mut module_name = None;
    let mut defects = KotlincDefects::Reproduce;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let (name, joined) = match argument.split_once('=') {
            Some((name, value)) if name.starts_with('-') => (name, Some(value.to_string())),
            _ => (argument.as_str(), None),
        };
        let mut value = || -> Result<String, String> {
            match &joined {
                Some(value) => Ok(value.clone()),
                None => arguments
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("option {name} needs a value")),
            }
        };
        match name {
            "-help" | "-h" => return Ok(Invocation::Help),
            "-library" | "-l" => libraries.push(PathBuf::from(value()?)),
            "-produce" | "-p" => match value()?.as_str() {
                "program" => {}
                "library" => {
                    return Err(
                        "-produce library is not supported yet: krusty's KLIBs do not \
                                carry function bodies; compile the library's sources into the \
                                program"
                            .to_string(),
                    )
                }
                other => {
                    return Err(format!(
                        "-produce {other} is not supported: krusty kotlinc-native produces a \
                         program"
                    ))
                }
            },
            "-output" | "-o" => output = Some(PathBuf::from(value()?)),
            "-target" => target = Some(value()?),
            "-module-name" => module_name = Some(value()?),
            // krusty never links the platform libraries implicitly: a compilation is always what
            // `-no-default-libs` makes.
            "-no-default-libs" | "-nodefaultlibs" => {}
            crate::cli::FIX_KOTLINC_DEFECTS if joined.is_none() => defects = KotlincDefects::Fix,
            other if other.starts_with('-') => {
                let occupancy = if SHARED_ARGUMENTS.contains(&other) {
                    crate::cli::argument_occupancy(argument)
                } else {
                    crate::cli::language_argument(argument)
                };
                match occupancy {
                    Some(LanguageArgument::Alone) => shared.push(argument.clone()),
                    Some(LanguageArgument::WithValue) => {
                        shared.push(argument.clone());
                        shared.push(value()?);
                    }
                    None => {
                        return Err(format!(
                            "option {other} is not supported by krusty kotlinc-native"
                        ))
                    }
                }
            }
            source => {
                has_sources = true;
                shared.push(source.to_string());
            }
        }
    }
    let target = match target.as_deref() {
        None | Some("host") => NativeTarget::host()
            .filter(|host| host.konan_name().is_some())
            .ok_or("this host is not a Kotlin/Native target; name one with -target")?,
        Some(name) => NativeTarget::from_konan_name(name).ok_or_else(|| {
            format!(
                "unknown target {name}; krusty kotlinc-native supports linux_x64 and linux_arm64"
            )
        })?,
    };
    if !has_sources {
        return Err("no source files".to_string());
    }
    let read = crate::cli::parse(shared);
    let problems = read
        .argument_errors
        .iter()
        .map(|error| crate::kotlinc_arguments::render(error))
        .chain(read.errors.iter().cloned())
        .chain(
            read.language_feature_problems
                .iter()
                .filter(|problem| problem.is_error)
                .map(|problem| crate::kotlinc_arguments::render(&problem.message)),
        )
        .chain(read.configuration_errors.iter().cloned())
        .collect::<Vec<_>>();
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    let sources = read
        .sources
        .iter()
        .map(|path| NativeSource {
            path: PathBuf::from(path),
            fragment: read.source_fragments.get(path).copied(),
            common: read.common_sources.contains(path),
        })
        .collect();
    let mut options = NativeCompileOptions::new(sources, target);
    options.features = read.language_settings.features;
    options.libraries = libraries;
    options.output = output;
    options.module_name = module_name;
    Ok(Invocation::Compile(Box::new(options), defects))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn kotlin_native_spellings_build_the_options() {
        let Invocation::Compile(options, defects) = parse(&arguments(
            "-p program -o out/app -target linux_arm64 -l a.klib -library=b -module-name app \
             -no-default-libs a.kt b.kt",
        ))
        .expect("parses") else {
            panic!("not a compilation");
        };
        assert_eq!(defects, KotlincDefects::Reproduce);
        let mut expected = NativeCompileOptions::new(
            vec![
                NativeSource::plain(PathBuf::from("a.kt")),
                NativeSource::plain(PathBuf::from("b.kt")),
            ],
            NativeTarget::from_konan_name("linux_arm64").expect("a target"),
        );
        expected.features = crate::cli::language_settings(Vec::<String>::new())
            .settings
            .features;
        expected.libraries = vec![PathBuf::from("a.klib"), PathBuf::from("b")];
        expected.output = Some(PathBuf::from("out/app"));
        expected.module_name = Some("app".to_string());
        assert_eq!(*options, expected);
    }

    #[test]
    fn a_shared_flag_stands_alone_and_a_shared_value_follows_its_option() {
        let Invocation::Compile(options, _) = parse(&arguments(
            "-Xexpect-actual-classes a.kt -opt-in kotlin.ExperimentalStdlibApi b.kt",
        ))
        .expect("parses") else {
            panic!("not a compilation");
        };
        assert_eq!(
            options.sources,
            vec![
                NativeSource::plain(PathBuf::from("a.kt")),
                NativeSource::plain(PathBuf::from("b.kt")),
            ]
        );
        let Invocation::Compile(options, _) = parse(&arguments(
            "a.kt -Xexpect-actual-classes -o app -Xexpect-actual-classes",
        ))
        .expect("parses") else {
            panic!("not a compilation");
        };
        assert_eq!(
            options.sources,
            vec![NativeSource::plain(PathBuf::from("a.kt"))]
        );
        assert_eq!(options.output, Some(PathBuf::from("app")));
    }

    #[test]
    fn fixing_kotlinc_defects_is_asked_by_name() {
        let Invocation::Compile(_, defects) =
            parse(&arguments("-Xfix-kotlinc-defects -target linux_x64 a.kt")).expect("parses")
        else {
            panic!("not a compilation");
        };
        assert_eq!(defects, KotlincDefects::Fix);
        assert_eq!(
            parse(&arguments(
                "-Xfix-kotlinc-defects=true -target linux_x64 a.kt"
            )),
            Err(
                "option -Xfix-kotlinc-defects is not supported by krusty kotlinc-native"
                    .to_string()
            )
        );
    }

    #[test]
    fn what_krusty_cannot_honor_is_refused_by_name() {
        assert_eq!(
            parse(&arguments("-g main.kt")),
            Err("option -g is not supported by krusty kotlinc-native".to_string())
        );
        assert_eq!(
            parse(&arguments("-p dynamic main.kt")),
            Err(
                "-produce dynamic is not supported: krusty kotlinc-native produces a program"
                    .to_string()
            )
        );
        assert_eq!(
            parse(&arguments("-target mingw_x64 main.kt")),
            Err(
                "unknown target mingw_x64; krusty kotlinc-native supports linux_x64 and \
                 linux_arm64"
                    .to_string()
            )
        );
        assert_eq!(
            parse(&arguments("-p library lib.kt")),
            Err(
                "-produce library is not supported yet: krusty's KLIBs do not carry function \
                 bodies; compile the library's sources into the program"
                    .to_string()
            )
        );
        assert_eq!(
            parse(&arguments("-nopack main.kt")),
            Err("option -nopack is not supported by krusty kotlinc-native".to_string())
        );
        assert_eq!(
            parse(&arguments("-o app")),
            Err("no source files".to_string())
        );
        assert_eq!(
            parse(&arguments("main.kt -l")),
            Err("option -l needs a value".to_string())
        );
        assert_eq!(parse(&arguments("-h")), Ok(Invocation::Help));
    }
}
