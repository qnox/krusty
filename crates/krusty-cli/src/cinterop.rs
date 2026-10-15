//! `krusty cinterop`: Kotlin/Native's `cinterop` tool, taking its options.
//!
//! The options are spelled as Kotlin/Native 2.4.20's `cinterop -help` lists them, and an option's
//! value follows it (`-o zlib`) or is joined to it by `=` (`-linker-option=-lz`). An option krusty
//! cannot honor is refused by name rather than ignored, since the library would silently differ.

use std::path::PathBuf;

use krusty::native::cinterop::CinteropOptions;
use krusty::native::NativeTarget;

pub const HELP: &str = "\
Usage: krusty cinterop options_list
Options:
    -def -> the library definition file { String }
    -header -> header file to produce kotlin bindings for { String }
    -pkg -> place generated bindings to the package { String }
    -output, -o [nativelib] -> specifies the resulting library file { String }
    -target [host] -> native target to compile to (linux_x64, linux_arm64) { String }
    -compiler-option -> additional compiler option { String }
    -compiler-options -> additional compiler options (allows to add several options separated by spaces) { String }
    -linker-option -> additional linker option { String }
    -linker-options -> additional linker options (allows to add several options separated by spaces) { String }
    -nopack [false] -> Don't pack the produced library into a klib file
    -no-default-libs [false] -> don't link the libraries from dist/klib automatically
    -Xmodule-name -> A full name of the library used for dependency resolution { String }
    -Xccall-mode [direct] -> CCall mode; krusty generates @CCall.Direct only { direct }
    -help, -h -> Usage info
";

/// What a `cinterop` command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Build(CinteropOptions),
}

/// Read the arguments that follow `krusty cinterop`.
pub fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut def = None;
    let mut headers = Vec::new();
    let mut package = None;
    let mut output = None;
    let mut target = None;
    let mut compiler_options = Vec::new();
    let mut linker_options = Vec::new();
    let mut no_pack = false;
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
            "-def" => def = Some(PathBuf::from(value()?)),
            "-header" => headers.push(value()?),
            "-pkg" => package = Some(value()?),
            "-output" | "-o" => output = Some(PathBuf::from(value()?)),
            "-target" => target = Some(value()?),
            "-compiler-option" => compiler_options.push(value()?),
            "-compiler-options" | "-compilerOpts" => {
                compiler_options.extend(value()?.split_whitespace().map(str::to_string))
            }
            "-linker-option" => linker_options.push(value()?),
            "-linker-options" | "-linkerOpts" => {
                linker_options.extend(value()?.split_whitespace().map(str::to_string))
            }
            "-nopack" => no_pack = true,
            // krusty's library never depends on the platform libraries: it is always what
            // `-no-default-libs` makes.
            "-no-default-libs" | "-nodefaultlibs" => {}
            "-Xmodule-name" => module_name = Some(value()?),
            "-Xccall-mode" => match value()?.as_str() {
                "direct" => {}
                other => {
                    return Err(format!(
                        "-Xccall-mode {other} is not supported: krusty calls C directly and \
                         generates @CCall.Direct only"
                    ))
                }
            },
            other if other.starts_with('-') => {
                return Err(format!(
                    "option {other} is not supported by krusty cinterop"
                ))
            }
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let target = match target.as_deref() {
        None | Some("host") => NativeTarget::host()
            .filter(|host| host.konan_name().is_some())
            .ok_or("this host is not a Kotlin/Native target; name one with -target")?,
        Some(name) => NativeTarget::from_konan_name(name).ok_or_else(|| {
            format!("unknown target {name}; krusty cinterop supports linux_x64 and linux_arm64")
        })?,
    };
    if def.is_none() && headers.is_empty() {
        return Err("cinterop needs a -def file or a -header".to_string());
    }
    let mut options =
        CinteropOptions::new(output.unwrap_or_else(|| PathBuf::from("nativelib")), target);
    options.def = def;
    options.headers = headers;
    options.package = package;
    options.compiler_options = compiler_options;
    options.linker_options = linker_options;
    options.no_pack = no_pack;
    options.module_name = module_name;
    Ok(Invocation::Build(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn kotlin_native_spellings_build_the_options() {
        let mut command = arguments(
            "-def zlib.def -o out/zlib -target linux_arm64 -pkg z -compiler-option -DX=1 \
             -linker-option=-lm -header extra.h -nopack -no-default-libs -Xccall-mode direct \
             -Xmodule-name zz -compiler-options",
        );
        command.push("-I/a -I/b".to_string());
        let Invocation::Build(options) = parse(&command).expect("parses") else {
            panic!("not a build");
        };
        let mut expected = CinteropOptions::new(
            PathBuf::from("out/zlib"),
            NativeTarget::from_konan_name("linux_arm64").expect("a target"),
        );
        expected.def = Some(PathBuf::from("zlib.def"));
        expected.headers = vec!["extra.h".to_string()];
        expected.package = Some("z".to_string());
        expected.compiler_options =
            vec!["-DX=1".to_string(), "-I/a".to_string(), "-I/b".to_string()];
        expected.linker_options = vec!["-lm".to_string()];
        expected.no_pack = true;
        expected.module_name = Some("zz".to_string());
        assert_eq!(options, expected);
    }

    #[test]
    fn what_krusty_cannot_honor_is_refused_by_name() {
        assert_eq!(
            parse(&arguments("-def z.def -staticLibrary libz.a")),
            Err("option -staticLibrary is not supported by krusty cinterop".to_string())
        );
        assert_eq!(
            parse(&arguments("-def z.def -Xccall-mode indirect")),
            Err(
                "-Xccall-mode indirect is not supported: krusty calls C directly and generates \
                 @CCall.Direct only"
                    .to_string()
            )
        );
        assert_eq!(
            parse(&arguments("-def z.def -Xccall-mode both")),
            Err(
                "-Xccall-mode both is not supported: krusty calls C directly and generates \
                 @CCall.Direct only"
                    .to_string()
            )
        );
        assert_eq!(
            parse(&arguments("-def z.def -target mingw_x64")),
            Err(
                "unknown target mingw_x64; krusty cinterop supports linux_x64 and linux_arm64"
                    .to_string()
            )
        );
        assert_eq!(
            parse(&arguments("-o z")),
            Err("cinterop needs a -def file or a -header".to_string())
        );
        assert_eq!(
            parse(&arguments("-def")),
            Err("option -def needs a value".to_string())
        );
        assert_eq!(parse(&arguments("-h")), Ok(Invocation::Help));
    }
}
