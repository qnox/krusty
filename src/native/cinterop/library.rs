//! A `.def` file and its headers to an interop library: a KLIB laid out as Kotlin/Native's
//! `cinterop -Xccall-mode direct` writes one, holding only metadata.
//!
//! The headers are read as one translation unit — an `#include` of each header the `.def` file
//! lists, then the `.def` file's own code — with its `compilerOpts` macros and include
//! directories. The declarations of the library's own headers, and everything they reference,
//! become Kotlin declarations ([`registry`], [`mirror`], then a module per kind of declaration);
//! [`emit`] writes them as package fragments, and the manifest records what the `.def` file said.
//!
//! The library depends only on the Kotlin standard library: a type from a header outside the
//! library's own is declared in the library too, as cinterop does without platform libraries.

mod callables;
mod emit;
mod enums;
mod headers;
mod kotlin_type;
mod macros;
mod mirror;
mod registry;
mod sha256;
mod spelling;
mod structs;
mod stub;
mod typedefs;

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use super::compiler_headers::preprocessor_for;
use super::declarations::parse_with_macros;
use super::def_file::{CompilerOptions, DefFile};
use super::model::Origin;
use super::preprocessor::IncludeDir;
use crate::klib::write::{KlibPlatform, KlibStamp, KlibWriter};
use crate::native::target::NativeTarget;
use callables::{function_stubs, global_property, Definition};
use headers::{header_id, header_name, HeaderFilter};
use mirror::{EnumStyles, Mapper};
use registry::Registry;
use stub::Package;

/// What to build an interop library from.
pub(crate) struct InteropRequest<'a> {
    pub(crate) def: &'a DefFile,
    /// The library's `unique_name`, which its module is named after.
    pub(crate) unique_name: &'a str,
    /// The package when the `.def` file names none.
    pub(crate) default_package: &'a str,
    pub(crate) target: NativeTarget,
    /// The C library's include directories, searched after the compiler's own headers.
    pub(crate) system_dirs: Vec<IncludeDir>,
    /// The target's sysroot, below which a header is named by its path.
    pub(crate) sysroot: Option<PathBuf>,
    pub(crate) stamp: KlibStamp,
    /// Headers given beside the `.def` file (`-header`), read after its own.
    pub(crate) extra_headers: Vec<String>,
    /// Compiler options given beside the `.def` file (`-compiler-option`), after its own.
    pub(crate) extra_compiler_options: Vec<String>,
}

/// Why an interop library could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CinteropError(pub(crate) String);

impl std::fmt::Display for CinteropError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CinteropError {}

/// The path the translation unit's main file is read as; the `.def` file's code is in it.
const MAIN_FILE: &str = "<def>";

/// Build the interop library `request` describes.
pub(crate) fn interop_library(request: InteropRequest<'_>) -> Result<KlibWriter, CinteropError> {
    let def = request.def;
    let Some(konan_target) = request.target.konan_name() else {
        return Err(CinteropError(format!(
            "Kotlin/Native has no interop library format for {}",
            request.target
        )));
    };
    let list = |key: &str| {
        def.list(key, "linux", konan_target)
            .map_err(|error| CinteropError(format!("{key}: {error}")))
    };
    let mut compiler_options = list("compilerOpts")?;
    compiler_options.extend(request.extra_compiler_options.iter().cloned());
    let options = CompilerOptions::parse(&compiler_options).map_err(CinteropError)?;
    let mut header_list = list("headers")?;
    header_list.extend(request.extra_headers.iter().cloned());
    let filter = HeaderFilter::new(list("headerFilter")?);
    let excluded: HashSet<String> = list("excludedFunctions")?.into_iter().collect();
    let strict = list("strictEnums")?;
    let non_strict = list("nonStrictEnums")?;
    let package = def
        .value("package", "linux", konan_target)
        .unwrap_or_else(|| request.default_package.to_string());

    let user_dirs: Vec<IncludeDir> = options
        .include_dirs
        .iter()
        .map(|dir| IncludeDir::Disk(PathBuf::from(dir)))
        .collect();
    let mut preprocessor = preprocessor_for(request.target, user_dirs, request.system_dirs.clone())
        .map_err(|error| CinteropError(error.to_string()))?;
    preprocessor
        .predefine(&options.predefines())
        .map_err(|error| CinteropError(error.to_string()))?;
    let mut main = String::new();
    for header in &header_list {
        main.push_str(&format!("#include <{header}>\n"));
    }
    main.push_str(&def.code);
    let tokens = preprocessor
        .run(MAIN_FILE, &main)
        .map_err(|error| CinteropError(error.to_string()))?;
    let dirs = preprocessor.include_dirs().to_vec();
    let own_file = |file: &str| {
        file == MAIN_FILE || header_name(&dirs, file).is_some_and(|name| filter.includes(&name))
    };

    // Every name an own header or the `.def` code defines a macro by, read once at the end: an
    // object-like macro as its expansion, a name `#undef` left as itself (it may now name a
    // function), a function-like macro not at all.
    let mut seen = HashSet::new();
    let mut macros = Vec::new();
    for (name, file) in preprocessor.definitions.clone() {
        if !file.as_deref().is_some_and(own_file) || !seen.insert(name.clone()) {
            continue;
        }
        if preprocessor
            .macros
            .get(&name)
            .is_some_and(|definition| definition.params.is_some())
        {
            continue;
        }
        let expansion = preprocessor
            .expand_name(&name)
            .map_err(|error| CinteropError(error.to_string()))?;
        macros.push((name, expansion));
    }
    let (header, constants) = parse_with_macros(&tokens, &macros, request.target)
        .map_err(|error| CinteropError(error.to_string()))?;
    let declarations = &header.declarations;
    let mut layouts = header.layouts;

    let own = |origin: &Origin| own_file(&origin.file);
    let registry = Registry::new(declarations, own);
    let mapper = Mapper::new(
        declarations,
        &registry,
        &package,
        &EnumStyles {
            strict: &strict,
            non_strict: &non_strict,
        },
    );

    let mut contents = Package::default();
    for &id in &registry.records {
        if declarations.record(id).fields.is_some() {
            if let Some(classes) = structs::record_classes(&mapper, &mut layouts, id) {
                contents.classes.push(classes);
            }
        }
    }
    for &id in &registry.enums {
        if mapper.is_strict(id) {
            if let Ok(classes) = enums::enum_classes(&mapper, id) {
                contents.classes.push(classes);
            }
        }
    }
    for &id in &registry.typedefs {
        contents
            .aliases
            .extend(typedefs::typedef_aliases(&mapper, id).unwrap_or_default());
    }
    for &id in &registry.enums {
        contents
            .aliases
            .extend(enums::enum_aliases(&mapper, id).unwrap_or_default());
    }
    for &index in &registry.functions {
        let function = &declarations.functions[index];
        if excluded.contains(&*function.name) {
            continue;
        }
        let definition = function.has_body.then(|| {
            if &*function.origin.file == MAIN_FILE {
                Definition::DefFile
            } else {
                Definition::Header
            }
        });
        if let Ok(stubs) = function_stubs(&mapper, function, definition) {
            contents.functions.extend(stubs);
        }
    }
    let mut globals: Vec<_> = registry
        .variables
        .iter()
        .map(|&index| &declarations.variables[index])
        .collect();
    globals.sort_by(|left, right| left.name.cmp(&right.name));
    let mut property_names: HashSet<Rc<str>> = HashSet::new();
    for variable in globals {
        if let Ok(property) = global_property(&mapper, variable) {
            property_names.insert(variable.name.clone());
            contents.properties.push(property);
        }
    }
    for mut property in macros::macro_properties(&mapper, &constants) {
        while property_names.contains(property.name.as_str()) {
            property.name.push('_');
        }
        contents.properties.push(property);
    }
    for &id in &registry.enums {
        contents
            .properties
            .extend(enums::enum_constants(&mapper, id));
    }

    let mut writer = KlibWriter::new(request.unique_name, request.stamp, KlibPlatform::Native);
    let segments: Vec<&str> = package.split('.').collect();
    for depth in 1..segments.len() {
        let parent = segments[..depth].join(".");
        writer.add_empty_package(&parent, emit::empty_fragment(&parent));
    }
    for fragment in emit::fragments(&package, &contents) {
        writer.add_fragment(&package, fragment);
    }

    for (key, value) in def.properties() {
        if key != "depends" {
            writer.set_property(key, value);
        }
    }
    let forward: Vec<String> = registry
        .records
        .iter()
        .filter(|&&id| declarations.record(id).fields.is_none())
        .map(|&id| format!("cnames.structs.{}", registry.record_name(id)))
        .collect();
    let mut included: Vec<String> = preprocessor
        .included
        .iter()
        .filter(|file| &***file != MAIN_FILE && own_file(file))
        .map(|file| header_id(&dirs, request.sysroot.as_deref(), file))
        .collect();
    if !filter.is_filtered() {
        included.push("builtins".to_string());
    }
    writer.set_property("depends", "stdlib");
    writer.set_property("exportForwardDeclarations", &forward.join(" "));
    writer.set_property("includedForwardDeclarations", &forward.join(" "));
    writer.set_property("includedHeaders", &included.join(" "));
    writer.set_property("interop", "true");
    writer.set_property("ir_provider", "kotlin.native.cinterop");
    writer.set_property("native_targets", konan_target);
    writer.set_property("package", &package);
    writer.add_directory("default/resources");
    writer.add_directory(&format!("default/targets/{konan_target}/included"));
    writer.add_directory(&format!("default/targets/{konan_target}/native"));
    Ok(writer)
}

#[cfg(test)]
mod tests;
