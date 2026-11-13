//! Build the pinned Native runner's foreign source modules with the stock cinterop CLI.
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

mod toolchain;
use toolchain::Tools;

#[derive(Debug, Default)]
pub(super) struct Modules {
    files: BTreeMap<String, BTreeMap<String, String>>,
    tools: Option<Tools>,
}

impl Modules {
    pub(super) fn new(source: &str, compiler: &Path) -> Result<Self, String> {
        let mut modules: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut module = None;
        let mut file = None;
        let mut body = String::new();
        let flush = |modules: &mut BTreeMap<String, BTreeMap<String, String>>,
                     module: &Option<String>,
                     file: &mut Option<String>,
                     body: &mut String|
         -> Result<(), String> {
            if let (Some(module), Some(file)) = (module, file.take()) {
                let previous = modules
                    .entry(module.clone())
                    .or_default()
                    .insert(file.clone(), std::mem::take(body));
                if previous.is_some() {
                    return Err(format!("duplicate file {file:?} in module {module:?}"));
                }
            } else {
                body.clear();
            }
            Ok(())
        };
        for line in source.lines() {
            let directive = line.trim_start();
            if let Some(header) = directive.strip_prefix("// MODULE:") {
                flush(&mut modules, &module, &mut file, &mut body)?;
                module = Some(header.split('(').next().unwrap().trim().to_owned());
            } else if let Some(name) = directive.strip_prefix("// FILE:") {
                flush(&mut modules, &module, &mut file, &mut body)?;
                file = Some(name.trim().to_owned());
            } else if file.is_some() {
                body.push_str(line);
                body.push('\n');
            }
        }
        flush(&mut modules, &module, &mut file, &mut body)?;
        modules.retain(|_, files| {
            files.keys().any(|file| file.ends_with(".def"))
                && !files.keys().any(|file| file.ends_with(".kt"))
        });
        let tools = if modules.is_empty() {
            None
        } else {
            Some(Tools::load(compiler)?)
        };
        Ok(Self {
            files: modules,
            tools,
        })
    }

    pub(super) fn contains(&self, module: &str) -> bool {
        self.files.contains_key(module)
    }

    pub(super) fn identity(&self) -> &str {
        self.tools
            .as_ref()
            .map_or("", |tools| tools.identity.as_str())
    }

    pub(super) fn compile(
        &self,
        compiler: &Path,
        source: &str,
        module: &str,
        root: &Path,
        libraries: &[PathBuf],
    ) -> Result<PathBuf, String> {
        // A cold cinterop includes native libclang alongside its JVM heap. Bound these processes
        // independently of the Kotlin compiler pool so corpus workers cannot exhaust CI memory.
        static ACTIVE: Mutex<()> = Mutex::new(());
        let _active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
        let files = self.files.get(module).ok_or("missing foreign module")?;
        let work = relative(root, module)?;
        std::fs::create_dir_all(&work).map_err(|error| error.to_string())?;
        let mut definitions = Vec::new();
        let mut sources = Vec::new();
        for (name, text) in files {
            let path = relative(&work, name)?;
            std::fs::create_dir_all(path.parent().ok_or("foreign file has no parent")?)
                .map_err(|error| error.to_string())?;
            std::fs::write(&path, text).map_err(|error| error.to_string())?;
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("def") => definitions.push(path),
                Some("c" | "cpp" | "m" | "mm") => sources.push(path),
                _ => {}
            }
        }
        let [definition] = definitions.as_slice() else {
            return Err(format!(
                "foreign module {module} must contain exactly one .def file"
            ));
        };
        let archive = work.join("output");
        let mut command = Command::new(compiler.with_file_name("cinterop"));
        command.current_dir(&work).args([
            "-J-Xmx1g",
            "-J-XX:+UseSerialGC",
            "-no-default-libs",
            "-nopack",
        ]);
        command.arg("-def").arg(definition).arg("-o").arg(&archive);
        command.arg("-target").arg(
            super::native_target()?
                .konan_name()
                .ok_or("unsupported Konan target")?,
        );
        command.arg("-Xmodule-name").arg(module);
        command
            .arg("-compiler-option")
            .arg(format!("-I{}", definition.parent().unwrap().display()));
        command.arg("-compiler-option").arg(format!(
            "-fmodules-cache-path={}",
            work.join("modulesCache").display()
        ));
        command
            .arg("-Xkotlinc-option")
            .arg(format!("-Xklib-relative-path-base={}", work.display()));
        for library in libraries {
            command.arg("-l").arg(library);
        }
        let tools = self.tools.as_ref().ok_or("missing Native SDK tools")?;
        for path in &sources {
            let archive = compile_archive(
                tools,
                path,
                &work,
                definition.parent().unwrap(),
                root,
                module,
            )?;
            command.arg("-libraryPath").arg(archive.parent().unwrap());
            command
                .arg("-staticLibrary")
                .arg(archive.file_name().unwrap());
        }
        for line in source.lines() {
            if let Some(arguments) = line.trim_start().strip_prefix("// FREE_CINTEROP_ARGS:") {
                command.args(arguments.trim().split(' ').map(|argument| {
                    argument.replace("$generatedSourcesDir", &root.to_string_lossy())
                }));
            }
        }
        run_foreign_compiler(&mut command, "cinterop", module)?;
        Ok(archive)
    }
}

fn relative(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    if name.is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "foreign source path must be relative to its module: {name:?}"
        ));
    }
    Ok(root.join(path))
}

fn compile_archive(
    tools: &Tools,
    source: &Path,
    work: &Path,
    include: &Path,
    root: &Path,
    module: &str,
) -> Result<PathBuf, String> {
    let cpp = source
        .extension()
        .is_some_and(|extension| extension == "cpp" || extension == "mm");
    let archive = source.with_extension("a");
    let object = source.with_extension("a.o");
    let mut clang = Command::new(if cpp { &tools.clangxx } else { &tools.clang });
    clang
        .current_dir(work)
        .args(if cpp { &tools.cpp } else { &tools.c });
    if cpp {
        clang.arg("-std=c++17");
    }
    clang
        .arg(
            source
                .strip_prefix(work)
                .map_err(|error| error.to_string())?,
        )
        .arg("-I")
        .arg(include);
    clang.args(["-g", "-lpthread", "-fno-objc-arc", "-c"]);
    // Native KLIB source paths and auxiliary DWARF share a stable module-relative base.
    clang
        .arg(format!("-fdebug-prefix-map={}=.", root.display()))
        .arg("-fdebug-compilation-dir=.");
    clang.arg("-o").arg(&object);
    run_foreign_compiler(&mut clang, "clang", module)?;
    run_foreign_compiler(
        Command::new(&tools.ar)
            .arg("-rc")
            .arg(&archive)
            .arg(&object),
        "llvm-ar",
        module,
    )?;
    Ok(archive)
}

fn run_foreign_compiler(command: &mut Command, phase: &str, module: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("start {phase} for {module}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{phase} rejected foreign module {module} ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
