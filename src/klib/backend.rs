//! The KLIB target: a module compiled to a Kotlin library instead of to code.
//!
//! A KLIB is what a non-JVM dependency is distributed as, and what a dependent compilation reads its
//! declarations from. This backend writes one `PackageFragment` per checked file, from the same
//! declaration records the JVM `@Metadata` is built from, and lays the fragments out as a library
//! ([`super::write`]). It runs before any target lowering: a library records Kotlin declarations,
//! not a target's realization of them.

use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::DiagSink;
use crate::ir::{IrClass, IrFile};
use crate::metadata::class_declarations::{ClassTailOptions, PropertyOrigin};
use crate::metadata::declaration_records;
use crate::metadata::klib_fragment::{package_fragment, KlibClass, KlibFileMembers};

use super::write::{KlibPlatform, KlibStamp, KlibWriter};

/// The prefix of every construct this backend declines; what follows names the construct.
pub const DECLINE_PREFIX: &str = "krusty: the KLIB writer does not support ";

/// Writes a module as a KLIB.
pub struct KlibBackend {
    platform: KlibPlatform,
    stamp: KlibStamp,
    annotations_in_metadata: bool,
}

impl KlibBackend {
    /// A library for `platform`, stamped with the compilation's finalized versions.
    pub fn new(platform: KlibPlatform, stamp: KlibStamp) -> Self {
        Self {
            platform,
            stamp,
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
        let classes = library_classes(ir);
        if let Some(construct) = classes
            .iter()
            .find_map(|&class| unsupported_class_construct(ir, class))
        {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("{DECLINE_PREFIX}{construct} yet"),
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
        // A library serializes each class's declaration record exactly as the handoff built it.
        let Some(records) = classes
            .iter()
            .map(|class| {
                ir.class_declarations
                    .get(&class.fq_name_id())
                    .and_then(Option::as_ref)
            })
            .collect::<Option<Vec<_>>>()
        else {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("{DECLINE_PREFIX}this class shape yet"),
            );
            return Vec::new();
        };
        let constants = records
            .iter()
            .map(|record| {
                record
                    .prop_origins
                    .iter()
                    .map(|origin| match origin {
                        PropertyOrigin::Constant(storage) => static_constant(ir, *storage),
                        PropertyOrigin::Declared(_)
                        | PropertyOrigin::MemberExtension(_)
                        | PropertyOrigin::CompanionBlock(_) => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let secondary_constructors = records
            .iter()
            .map(|record| record.secondary_constructors())
            .collect::<Vec<_>>();
        let nested = records
            .iter()
            .map(|record| record.nested())
            .collect::<Vec<_>>();
        let enum_entries = records
            .iter()
            .map(|record| record.enum_entries())
            .collect::<Vec<_>>();
        let tails = records
            .iter()
            .zip(secondary_constructors.iter().zip(&nested))
            .map(|(record, (secondary, nested))| {
                record.tail(
                    ClassTailOptions {
                        param_assertions: false,
                        annotations_in_metadata: self.annotations_in_metadata,
                        compiler_version_requirement: None,
                        intersection: None,
                        primary_ctor_annotations: None,
                    },
                    secondary,
                    nested,
                )
            })
            .collect::<Vec<_>>();
        let declarations = records
            .iter()
            .zip(&tails)
            .zip(enum_entries.iter().zip(&constants))
            .map(|((record, tail), (entries, constants))| KlibClass {
                declaration: record.declaration(tail, entries),
                constants,
            })
            .collect::<Vec<_>>();
        let fragment = package_fragment(
            &segments,
            &members,
            &declarations,
            self.annotations_in_metadata,
        );
        state.fragments.push((package, fragment));
        Vec::new()
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        let mut writer = KlibWriter::new(module_name, self.stamp, self.platform);
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

/// The classes a library records for a file, in fragment order: each source-declared member
/// classifier after the classifier it is nested in, siblings in declaration order. A classifier
/// declared in executable code is no declaration of the library.
fn library_classes(ir: &IrFile) -> Vec<&IrClass> {
    let mut members = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, class)| {
            class.is_source_declared
                && class.enclosure.is_none()
                && !class.is_local_class
                && !class.is_anonymous_object
                && !class.is_enum_entry
        })
        .map(|(index, class)| {
            let order = ir
                .class_source_order(index as crate::ir::ClassId)
                .expect("a source classifier carries its stable declaration order");
            (order, class)
        })
        .collect::<Vec<_>>();
    members.sort_by_key(|(order, _)| *order);
    let mut ordered = Vec::with_capacity(members.len());
    let mut pending = members
        .iter()
        .rev()
        .filter(|(_, class)| class.fq_name.nested_owner().is_none())
        .map(|(_, class)| *class)
        .collect::<Vec<_>>();
    while let Some(class) = pending.pop() {
        ordered.push(class);
        pending.extend(
            members
                .iter()
                .rev()
                .filter(|(_, nested)| nested.fq_name.nested_owner() == Some(class.fq_name))
                .map(|(_, nested)| *nested),
        );
    }
    ordered
}

/// A construct of `class` this writer does not record yet.
fn unsupported_class_construct(ir: &IrFile, class: &IrClass) -> Option<&'static str> {
    if ir
        .generated_member_publication(class.fq_name_id())
        .is_some()
    {
        return Some("compiler-plugin members");
    }
    if ir
        .companion_blocks
        .properties_of(class.fq_name_id())
        .next()
        .is_some()
    {
        return Some("companion blocks");
    }
    if class.is_value {
        return Some("value classes");
    }
    None
}

/// The value a class `const val`'s static storage is initialized with.
fn static_constant(ir: &IrFile, storage: u32) -> Option<crate::ir::IrConst> {
    let init = ir.statics.get(storage as usize)?.init?;
    match &ir.exprs[init as usize] {
        crate::ir::IrExpr::Const(constant) => Some(constant.clone()),
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
    static_constant(ir, *storage)
}
