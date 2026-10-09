//! The KLIB target: a module compiled to a Kotlin library instead of to code.
//!
//! A KLIB is what a non-JVM dependency is distributed as, and what a dependent compilation reads its
//! declarations from. This backend writes one `PackageFragment` per checked file, from the same
//! declaration records the JVM `@Metadata` is built from, and lays the fragments out as a library
//! ([`super::write`]). It runs before any target lowering: a library records Kotlin declarations,
//! not a target's realization of them.

use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::DiagSink;
use crate::metadata::declaration_records;
use crate::metadata::klib_fragment::{package_fragment, KlibFileMembers};

use super::write::{KlibPlatform, KlibWriter};

/// The prefix of every construct this backend declines; what follows names the construct.
pub const DECLINE_PREFIX: &str = "krusty: the KLIB writer does not support ";

/// Writes a module as a KLIB.
pub struct KlibBackend {
    platform: KlibPlatform,
    depends: Vec<String>,
    annotations_in_metadata: bool,
}

impl KlibBackend {
    /// A library for `platform` whose declarations reference `depends` (the unique names of the
    /// libraries it was compiled against, `stdlib` first).
    pub fn new(platform: KlibPlatform, depends: Vec<String>) -> Self {
        Self {
            platform,
            depends,
            annotations_in_metadata: true,
        }
    }

    /// kotlinc's `AnnotationsInMetadata` language feature (on from Kotlin 2.4).
    pub fn with_annotations_in_metadata(mut self, enabled: bool) -> Self {
        self.annotations_in_metadata = enabled;
        self
    }
}

/// What the backend accumulates across the files of one module: each file's fragment and the
/// package it declares into, in compilation order.
#[derive(Default)]
pub struct KlibModule {
    fragments: Vec<(String, Vec<u8>)>,
}

impl Backend for KlibBackend {
    type State = KlibModule;

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let ir = &file.ir;
        if !ir.classes.is_empty() {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("{DECLINE_PREFIX}classes yet"),
            );
            return Vec::new();
        }
        let stem = &file.stems[file.source.raw() as usize];
        // Fragments record the file's simple name; a logical source path keeps only its last part.
        let simple_stem = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
        let members = KlibFileMembers {
            file_name: format!("{simple_stem}.kt"),
            functions: ir
                .package_functions
                .iter()
                .map(|declaration| declaration_records::package_function(ir, declaration))
                .collect(),
            properties: ir
                .package_properties
                .iter()
                .map(|declaration| {
                    declaration_records::package_property(
                        ir,
                        declaration,
                        declared_setter(ir, declaration),
                    )
                })
                .collect(),
            constants: ir
                .package_properties
                .iter()
                .map(|declaration| constant_value(ir, declaration))
                .collect(),
            aliases: ir
                .package_type_aliases
                .iter()
                .map(declaration_records::package_alias)
                .collect(),
        };
        let package = ir.package.clone().unwrap_or_default();
        let segments = package
            .split('.')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let fragment = package_fragment(&segments, &members, self.annotations_in_metadata);
        state.fragments.push((package, fragment));
        Vec::new()
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        let mut writer = KlibWriter::new(
            module_name,
            crate::kotlin_version::target(),
            &self.platform,
            &self.depends,
        );
        for (package, fragment) in state.fragments {
            writer.add_fragment(&package, fragment);
        }
        writer.entries()
    }
}

/// The common-IR function of a property's source-declared setter, whose parameter name the
/// declaration record keeps.
fn declared_setter(
    ir: &crate::ir::IrFile,
    declaration: &crate::ir::IrPackageProperty,
) -> Option<u32> {
    match ir.local_property_layouts.get(&declaration.property)? {
        crate::ir::IrLocalPropertyLayout::TopLevelStorage { setter, .. }
        | crate::ir::IrLocalPropertyLayout::TopLevelAccessor { setter, .. } => *setter,
        _ => None,
    }
}

/// A `const val`'s value: the constant its storage is initialized with.
fn constant_value(
    ir: &crate::ir::IrFile,
    declaration: &crate::ir::IrPackageProperty,
) -> Option<crate::ir::IrConst> {
    if !declaration.is_const {
        return None;
    }
    let crate::ir::IrLocalPropertyLayout::TopLevelStorage { storage, .. } =
        ir.local_property_layouts.get(&declaration.property)?
    else {
        return None;
    };
    let init = ir.statics.get(*storage as usize)?.init?;
    match &ir.exprs[init as usize] {
        crate::ir::IrExpr::Const(constant) => Some(constant.clone()),
        _ => None,
    }
}
