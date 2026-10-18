//! The KLIB target: a module compiled to a Kotlin library instead of to code.
//!
//! A KLIB is what a non-JVM dependency is distributed as, and what a dependent compilation reads its
//! declarations from. This backend writes one `PackageFragment` per checked file, from the same
//! declaration records the JVM `@Metadata` is built from, and lays the fragments out as a library
//! ([`super::write`]). Supported inline templates additionally use Kotlin 2.4's standard prepared
//! inline-function IR component. Both run before target lowering: a library records checked Kotlin
//! declarations and target-neutral inline bodies, not a target's realization of them.

use crate::backend::{
    Artifact, Backend, CheckedIrFile, DependencyExportSignatures, DependencyMetadata,
};
use crate::compilation_target::CompilationTarget;
use crate::diag::DiagSink;
use crate::ir::{IrClass, IrFile};
use crate::metadata::class_declarations::{ClassTailOptions, FunctionOrigin, PropertyOrigin};
use crate::metadata::declaration_records;
use crate::metadata::id_signature::{
    package_function_signature, package_property_accessor_signature, MetadataAccessor,
    MetadataClass, MetadataContainer,
};
use crate::metadata::klib_fragment::{
    package_fragment_with_inner_classifiers, KlibClass, KlibFileMembers,
};
use crate::metadata::semantic::parse_package_fragment_checked;

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

    fn target(&self) -> CompilationTarget {
        match self.platform {
            KlibPlatform::Native => CompilationTarget::Native,
            KlibPlatform::WasmJs => CompilationTarget::WasmJs,
            KlibPlatform::WasmWasi => CompilationTarget::WasmWasi,
        }
    }

    fn record_file(
        &self,
        file: &CheckedIrFile<'_>,
        state: &mut KlibModule,
        diags: &mut DiagSink,
    ) -> DependencyExportSignatures {
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
            return DependencyExportSignatures::default();
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
            return DependencyExportSignatures::default();
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
        let inner_classifier = |classifier| {
            crate::metadata::inner_classifiers::inner_classifier(ir, &file.classifiers, classifier)
        };
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
                        inner_classifiers: &inner_classifier,
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
                in_source_file: record.has_source_file(),
            })
            .collect::<Vec<_>>();
        let fragment = package_fragment_with_inner_classifiers(
            &segments,
            &members,
            &declarations,
            self.annotations_in_metadata,
            &inner_classifier,
        );
        let Some(package_exports) =
            package_exports(ir, file.module_name, &segments, &fragment, diags)
        else {
            return DependencyExportSignatures::default();
        };
        state.inline_functions.extend(super::inline_ir::collect(
            ir,
            &format!("{simple_stem}.kt"),
            &package_exports.declarations,
        ));
        state.fragments.push((package, fragment));
        package_exports.executables
    }

    fn finish(&self, state: KlibModule, module_name: &str) -> Vec<Artifact> {
        let mut writer = KlibWriter::new(module_name, self.stamp, self.platform);
        for (package, fragment) in state.fragments {
            writer.add_fragment(&package, fragment);
        }
        let inline_ir = super::inline_ir::artifacts(&state.inline_functions);
        if !inline_ir.is_empty() {
            // The serialized types use Kotlin built-ins. This is the standard module edge a full
            // KLIB records; metadata-only libraries did not previously need one.
            writer.set_property(
                "depends",
                match self.platform {
                    KlibPlatform::Native => "stdlib",
                    KlibPlatform::WasmJs | KlibPlatform::WasmWasi => "kotlin",
                },
            );
        }
        for (path, bytes) in inline_ir {
            writer.add_file(&path, bytes);
        }
        writer.entries()
    }
}

/// What the backend accumulates across the files of one module: each file's fragment and the
/// package it declares into, in compilation order.
#[derive(Default)]
pub struct KlibModule {
    fragments: Vec<(String, Vec<u8>)>,
    inline_functions: Vec<super::inline_ir::PreparedFunction>,
}

impl Backend for KlibBackend {
    type State = KlibModule;

    fn compilation_target(&self) -> CompilationTarget {
        self.target()
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let _ = self.record_file(&file, state, diags);
        Vec::new()
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        self.finish(state, module_name)
    }
}

impl DependencyMetadata for KlibBackend {
    type State = KlibModule;

    fn compilation_target(&self) -> CompilationTarget {
        self.target()
    }

    fn record_ir_file(
        &self,
        file: &CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> DependencyExportSignatures {
        self.record_file(file, state, diags)
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        self.finish(state, module_name)
    }
}

/// Exact callable identities published by this file's generated fragment.
///
/// Decode the bytes the dependency will actually read, then sign those declarations through the
/// same identity algorithm as `KlibLibraries`. Declaration order joins the decoded metadata back
/// to the stable common-IR callable ids that produced it; no spelling participates in the join.
struct PackageExports {
    executables: DependencyExportSignatures,
    declarations: super::inline_ir::DeclarationSignatures,
}

fn package_exports(
    ir: &IrFile,
    module_name: &str,
    package: &[&str],
    fragment: &[u8],
    diags: &mut DiagSink,
) -> Option<PackageExports> {
    let decoded = match parse_package_fragment_checked(fragment) {
        Ok(decoded) => decoded,
        Err(error) => {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: cannot decode generated KLIB metadata: {error}"),
            );
            return None;
        }
    };
    if decoded.functions.len() != ir.package_functions.len()
        || decoded.properties.len() != ir.package_properties.len()
    {
        diags.error(
            crate::diag::Span::new(0, 0),
            "internal error: generated KLIB metadata changed package declaration order",
        );
        return None;
    }
    let package = package
        .iter()
        .map(|segment| (*segment).to_string())
        .collect::<Vec<_>>();
    let container = MetadataContainer {
        package: &package,
        classes: &[],
        native_interop_library: false,
    };
    let mut exports = DependencyExportSignatures::default();
    let mut declarations = super::inline_ir::DeclarationSignatures::default();
    for (source, declaration) in ir.package_functions.iter().zip(&decoded.functions) {
        if source.visibility == crate::types::Visibility::Private {
            continue;
        }
        let signature = match package_function_signature(container, declaration) {
            Ok(signature) => signature,
            Err(error) => {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("internal error: cannot sign generated KLIB function: {error}"),
                );
                return None;
            }
        };
        declarations.record(source.function, signature.clone(), None);
        // Metadata retains inline templates and bodyless declarations because a dependent
        // frontend needs their semantic declarations (and, for an inline function, its KLIB
        // body). The target registry is narrower: it may publish only code this source unit
        // actually emits.
        if crate::backend::has_standalone_dependency_code(ir, source.function) {
            exports.record_callable(
                source.function,
                crate::libraries::KlibDependencyCallable {
                    module: module_name.to_owned(),
                    declaration: crate::libraries::KlibDeclarationSignature::Public(signature),
                },
            );
        }
    }
    for (source, declaration) in ir.package_properties.iter().zip(&decoded.properties) {
        if source.visibility == crate::types::Visibility::Private {
            continue;
        }
        let Some(layout) = ir.local_property_layouts.get(&source.property) else {
            continue;
        };
        let (getter, setter) = match layout {
            crate::ir::IrLocalPropertyLayout::TopLevelStorage { getter, setter, .. } => {
                (*getter, *setter)
            }
            crate::ir::IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. } => {
                (Some(*getter), *setter)
            }
            crate::ir::IrLocalPropertyLayout::Member { .. }
            | crate::ir::IrLocalPropertyLayout::MemberExtension { .. } => continue,
        };
        for (callable, accessor) in [
            getter.map(|getter| (getter, MetadataAccessor::Getter)),
            setter.map(|setter| (setter, MetadataAccessor::Setter)),
        ]
        .into_iter()
        .flatten()
        {
            let signature =
                match package_property_accessor_signature(container, declaration, accessor) {
                    Ok(signature) => signature,
                    Err(error) => {
                        diags.error(
                            crate::diag::Span::new(0, 0),
                            format!(
                            "internal error: cannot sign generated KLIB property accessor: {error}"
                        ),
                        );
                        return None;
                    }
                };
            if crate::backend::has_standalone_dependency_code(ir, callable) {
                exports.record_callable(
                    callable,
                    crate::libraries::KlibDependencyCallable {
                        module: module_name.to_owned(),
                        declaration: crate::libraries::KlibDeclarationSignature::Accessor(
                            signature,
                        ),
                    },
                );
            }
        }
    }
    if let Err(error) = record_class_function_exports(
        ir,
        module_name,
        &package,
        &decoded,
        &mut exports,
        &mut declarations,
    ) {
        diags.error(
            crate::diag::Span::new(0, 0),
            format!("internal error: cannot sign generated KLIB class function: {error}"),
        );
        return None;
    }
    Some(PackageExports {
        executables: exports,
        declarations,
    })
}

/// Record the target bodies behind the exact member-function identities this fragment publishes.
///
/// The declaration record and decoded fragment retain the same metadata order. Joining them here
/// keeps the source `FunId` beside the identity a dependent provider will reconstruct, without
/// asking a backend to recover a member from its spelling or signature.
fn record_class_function_exports(
    ir: &IrFile,
    module_name: &str,
    package: &[String],
    decoded: &crate::metadata::semantic::KotlinPackage,
    exports: &mut DependencyExportSignatures,
    declarations: &mut super::inline_ir::DeclarationSignatures,
) -> Result<(), String> {
    let source_classes = library_classes(ir);
    if source_classes.len() != decoded.class_order.len() {
        return Err("generated KLIB metadata changed class declaration order".to_string());
    }
    for (source, name) in source_classes.into_iter().zip(&decoded.class_order) {
        let record = ir
            .class_declarations
            .get(&source.fq_name_id())
            .and_then(Option::as_ref)
            .ok_or_else(|| format!("class {} has no declaration record", source.fq_name()))?;
        let class = decoded
            .classes
            .get(name)
            .ok_or_else(|| format!("decoded class {name} is absent"))?;
        if class.functions.len() != record.method_origins.len() {
            return Err(format!(
                "class {name} changed function declaration order ({} decoded, {} recorded)",
                class.functions.len(),
                record.method_origins.len()
            ));
        }
        let Some(chain) = metadata_class_chain(package, name, &decoded.classes)? else {
            continue;
        };
        let container = MetadataContainer {
            package,
            classes: &chain,
            native_interop_library: false,
        };
        for (declaration, origin) in class.functions.iter().zip(&record.method_origins) {
            if declaration.visibility == crate::types::Visibility::Private {
                continue;
            }
            let FunctionOrigin::Declared(callable) = origin else {
                continue;
            };
            let function = ir
                .checked_callable_functions
                .get(callable)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "class function {} has no IR body identity",
                        declaration.name
                    )
                })?;
            let signature = package_function_signature(container, declaration)
                .map_err(|error| format!("{name}.{}: {error}", declaration.name))?;
            let receiver = if declaration.is_static {
                None
            } else {
                let receiver = ir.functions[function as usize]
                    .dispatch_receiver
                    .ok_or_else(|| {
                        format!(
                            "class function {} has no dispatch receiver",
                            declaration.name
                        )
                    })?;
                Some(crate::types::Ty::Obj(receiver, &[]))
            };
            // The current prepared writer can serialize a non-generic dispatch classifier exactly.
            // A generic owner also needs its class parameters remapped into the synthetic prepared
            // root; do not publish a raw receiver that would erase that declaration contract.
            if source.type_params.is_empty() && source.captured_type_params.is_empty() {
                declarations.record(function, signature.clone(), receiver);
            }
            if !crate::backend::has_standalone_dependency_code(ir, function) {
                continue;
            }
            exports.record_callable(
                function,
                crate::libraries::KlibDependencyCallable {
                    module: module_name.to_owned(),
                    declaration: crate::libraries::KlibDeclarationSignature::Public(signature),
                },
            );
            if let Some(receiver) = receiver {
                exports.record_member_receiver(function, receiver);
            }
        }
    }
    Ok(())
}

/// The metadata classifier scopes around `name`, outermost first and including `name`.
fn metadata_class_chain<'a>(
    package: &[String],
    name: &'a str,
    classes: &'a crate::fx_hash::HashMap<String, crate::metadata::semantic::KotlinClass>,
) -> Result<Option<Vec<MetadataClass<'a>>>, String> {
    let prefix = if package.is_empty() {
        String::new()
    } else {
        format!("{}/", package.join("/"))
    };
    let local = name
        .strip_prefix(&prefix)
        .ok_or_else(|| format!("class {name} lies outside its package"))?;
    let segments = local.split('.').collect::<Vec<_>>();
    let mut chain = Vec::with_capacity(segments.len());
    for depth in 0..segments.len() {
        let key = format!("{prefix}{}", segments[..=depth].join("."));
        let class = classes
            .get(&key)
            .ok_or_else(|| format!("enclosing class {key} is absent from its fragment"))?;
        if class.visibility == crate::types::Visibility::Private {
            return Ok(None);
        }
        chain.push(MetadataClass::of(segments[depth], class));
    }
    Ok(Some(chain))
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

/// The classes a library records for a file, in fragment order: each top-level source classifier
/// in declaration order, each followed by the classifiers its `nestedClassName` list names (the
/// declared ones, those a compiler plugin generated, the companion), in that list's order. A
/// classifier declared in executable code is no declaration of the library.
fn library_classes(ir: &IrFile) -> Vec<&IrClass> {
    let mut top_level = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, class)| {
            class.is_source_declared
                && class.enclosure.is_none()
                && !class.is_local_class
                && !class.is_anonymous_object
                && !class.is_enum_entry
                && class.fq_name.nested_owner().is_none()
        })
        .map(|(index, class)| {
            let order = ir
                .class_source_order(index as crate::ir::ClassId)
                .expect("a source classifier carries its stable declaration order");
            (order, class)
        })
        .collect::<Vec<_>>();
    top_level.sort_by_key(|(order, _)| *order);
    let mut ordered = Vec::with_capacity(ir.classes.len());
    let mut pending = top_level
        .into_iter()
        .rev()
        .map(|(_, class)| class)
        .collect::<Vec<_>>();
    while let Some(class) = pending.pop() {
        ordered.push(class);
        pending.extend(
            crate::metadata::class_declarations::nested_classifiers(ir, class)
                .iter()
                .rev()
                .map(|segment| {
                    let id = ir
                        .class_id_by_name(class.fq_name.nested_child(segment))
                        .expect("a listed nested classifier is declared in its owner's file");
                    &ir.classes[id as usize]
                }),
        );
    }
    ordered
}

/// A construct of `class` this writer does not record yet.
fn unsupported_class_construct(ir: &IrFile, class: &IrClass) -> Option<&'static str> {
    if ir
        .companion_blocks
        .properties_of(class.fq_name_id())
        .next()
        .is_some()
    {
        return Some("companion blocks");
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
