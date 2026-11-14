//! The pinned Native compiler supplies its LLVM paths and target-specific clang arguments.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::super::super::common::{self, byte_dump};
use super::super::Scratch;

#[derive(Clone, Debug)]
pub(super) struct Tools {
    pub(super) clang: PathBuf,
    pub(super) clangxx: PathBuf,
    pub(super) ar: PathBuf,
    pub(super) c: Vec<String>,
    pub(super) cpp: Vec<String>,
    pub(super) identity: String,
}

impl Tools {
    pub(super) fn load(compiler: &Path) -> Result<Self, String> {
        static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Tools>>> = OnceLock::new();
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        if let Some(tools) = cache.get(compiler) {
            return Ok(tools.clone());
        }
        let root = compiler
            .parent()
            .and_then(Path::parent)
            .ok_or("missing Native compiler root")?;
        let target = super::super::native_target()?
            .konan_name()
            .ok_or("unsupported Konan target")?;
        let directory = Scratch::new()?;
        let path = directory.0.join("NativeInteropTools.java");
        let source = include_str!("NativeInteropTools.java")
            .replace(
                "@ROOT_HEX@",
                &hex(root.to_str().ok_or("non-UTF8 Native root")?.as_bytes()),
            )
            .replace("@TARGET@", target);
        std::fs::write(&path, source).map_err(|error| error.to_string())?;
        let jar = root.join("konan/lib/kotlin-native-compiler-embeddable.jar");
        let output = common::javac_run(
            path.to_str().ok_or("non-UTF8 Native tool probe path")?,
            jar.to_str().ok_or("non-UTF8 Native compiler jar")?,
            directory
                .0
                .to_str()
                .ok_or("non-UTF8 Native tool probe directory")?,
            "NativeInteropTools",
        )
        .ok_or("Native clang configuration requires the persistent Java runner")?;
        let payload = output
            .split_once("NATIVE_CLANG_TOOLS\n")
            .ok_or_else(|| format!("Native clang configuration failed: {output}"))?
            .1;
        let mut values: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for line in payload.lines() {
            let (kind, value) = line
                .split_once(':')
                .ok_or_else(|| format!("invalid Native clang configuration: {line:?}"))?;
            if !matches!(kind, "clang" | "clangxx" | "ar" | "input" | "c" | "cpp") {
                return Err(format!(
                    "unknown Native clang configuration field: {kind:?}"
                ));
            }
            values.entry(kind).or_default().push(unhex(value)?);
        }
        let mut one = |kind| {
            let values = values.remove(kind).unwrap_or_default();
            match values.as_slice() {
                [value] => Ok(PathBuf::from(value)),
                _ => Err(format!("Native clang configuration requires one {kind}")),
            }
        };
        let clang = one("clang")?;
        let clangxx = one("clangxx")?;
        let ar = one("ar")?;
        let inputs = values
            .remove("input")
            .ok_or("Native clang configuration omitted SDK inputs")?
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let c = values
            .remove("c")
            .ok_or("Native clang configuration omitted C arguments")?;
        let cpp = values
            .remove("cpp")
            .ok_or("Native clang configuration omitted C++ arguments")?;
        let jdk = krusty::toolchain::jdk_modules().ok_or("no producing JDK modules")?;
        let digest = byte_dump::immutable_classpath_content_fingerprint(&inputs, &jdk)?;
        let identity = format!("{digest}/{clang:?}/{clangxx:?}/{ar:?}/{c:?}/{cpp:?}");
        let tools = Self {
            clang,
            clangxx,
            ar,
            c,
            cpp,
            identity,
        };
        cache.insert(compiler.into(), tools.clone());
        Ok(tools)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("write a String");
    }
    result
}

fn unhex(value: &str) -> Result<String, String> {
    if !value.len().is_multiple_of(2) || !value.is_ascii() {
        return Err("invalid hex in Native clang configuration".into());
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&value[offset..offset + 2], 16).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}
