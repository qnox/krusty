//! `krusty kotlinc-native`: Kotlin/Native's `kotlinc-native` compiler, taking its options.
//!
//! It produces a program. A library is not written yet, because krusty's KLIBs do not carry
//! ordinary function bodies: a first-party library's sources are compiled into the program instead.
//!
//! The options are spelled as Kotlin/Native 2.4.20's `kotlinc-native -help` lists them, and an
//! option's value follows it (`-o app`) or is joined to it by `=` (`-produce=library`). An option
//! krusty cannot honor is refused by name rather than ignored, since the result would silently
//! differ.

use std::path::PathBuf;

use krusty::native::{NativeCompileOptions, NativeTarget};

pub const HELP: &str = "\
Usage: krusty kotlinc-native <options> <source files>
Options:
    -library, -l <path> -> link with the library (a .klib or an unpacked library directory)
    -produce, -p {program} -> what to produce [program]
    -output, -o <name> -> the program to write; .kexe is appended [program]
    -target <target> -> native target to compile to (linux_x64, linux_arm64) [host]
    -module-name <name> -> the program's module name [the output's file name]
    -no-default-libs -> don't link the libraries from dist/klib automatically
    -help, -h -> usage info
";

/// What a `kotlinc-native` command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Compile(NativeCompileOptions),
}

/// Read the arguments that follow `krusty kotlinc-native`.
pub fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut sources = Vec::new();
    let mut libraries = Vec::new();
    let mut output = None;
    let mut target = None;
    let mut module_name = None;
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
            other if other.starts_with('-') => {
                return Err(format!(
                    "option {other} is not supported by krusty kotlinc-native"
                ))
            }
            source => sources.push(PathBuf::from(source)),
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
    if sources.is_empty() {
        return Err("no source files".to_string());
    }
    let mut options = NativeCompileOptions::new(sources, target);
    options.libraries = libraries;
    options.output = output;
    options.module_name = module_name;
    Ok(Invocation::Compile(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn kotlin_native_spellings_build_the_options() {
        let Invocation::Compile(options) = parse(&arguments(
            "-p program -o out/app -target linux_arm64 -l a.klib -library=b -module-name app \
             -no-default-libs a.kt b.kt",
        ))
        .expect("parses") else {
            panic!("not a compilation");
        };
        let mut expected = NativeCompileOptions::new(
            vec![PathBuf::from("a.kt"), PathBuf::from("b.kt")],
            NativeTarget::from_konan_name("linux_arm64").expect("a target"),
        );
        expected.libraries = vec![PathBuf::from("a.klib"), PathBuf::from("b")];
        expected.output = Some(PathBuf::from("out/app"));
        expected.module_name = Some("app".to_string());
        assert_eq!(options, expected);
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
