//! kotlinc's `AnonymousObjectTransformer`: an anonymous object an inline function's body creates is
//! copied, for each call site the body is inlined into, as a class of the caller's
//! (`<caller>$<function>$$inlined$<callee>$<n>`).
//!
//! The copy keeps the original's declarations and bodies with every reference to the original
//! renamed, regenerates its constructor and captured fields, records the original's name in its
//! `@Metadata`, points `EnclosingMethod` at the calling method, and carries a source map that
//! still names the original's source. Its members are written in the order the transformer visits
//! them, which is what fixes the order of the copy's constant pool.
//!
//! An object whose constructor is passed one of the call's inline lambdas (a `crossinline` one)
//! inlines it: the copy keeps the lambda's captured values instead of the lambda, and each of its
//! methods that called the lambda runs the lambda's body in its place (see `lambda_inlining`).
//! Objects whose bodies create anonymous objects of their own are a later stage (see "JVM unified
//! inliner" in `docs/IMPLEMENTATION_PLAN.md`).

mod constructor;
use constructor::DeclaredCapture;
mod lambda_inlining;
use lambda_inlining::FieldLambda;
mod type_remapper;

use std::collections::HashMap;
use std::rc::Rc;

use crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors;
use crate::jvm::class_node::{Annotation, ClassMethod, ClassNode, ElementValue, FieldNode};
use crate::jvm::classfile::{ClassWriter, CopyError, InnerClassSpec};
use crate::jvm::metadata::anonymous_origin::{record_origin_name, MetadataStrings};
use crate::jvm::metadata::MetadataDecodeError;
use crate::jvm::method_node::{Insn, MethodNode, Node};
use crate::jvm::source_map::{DependencyMap, SourceMap};

use super::class_roles::ClassRoles;
use super::{InlineError, ObjectLambda};

pub(crate) use type_remapper::{MalformedType, TypeRemapper};

const ALOAD: u8 = 0x19;
const GETFIELD: u8 = 0xb4;
const PUTFIELD: u8 = 0xb5;
const GETSTATIC: u8 = 0xb2;
const INVOKESTATIC: u8 = 0xb8;
const INVOKESPECIAL: u8 = 0xb7;
const NEW: u8 = 0xbb;
const INTRINSICS: &str = "kotlin/jvm/internal/Intrinsics";

/// `ACC_SYNTHETIC | ACC_FINAL`, a regenerated captured field's access
/// (`NO_FLAG_PACKAGE_PRIVATE | ACC_SYNTHETIC | ACC_FINAL`).
const CAPTURED_FIELD_ACCESS: u16 = 0x1010;
/// `JvmAnnotationNames.METADATA_PUBLIC_ABI_FLAG`.
const METADATA_PUBLIC_ABI_FLAG: i32 = 1 << 7;
const METADATA_DESC: &str = "Lkotlin/Metadata;";
const DEBUG_METADATA_DESC: &str = "Lkotlin/coroutines/jvm/internal/DebugMetadata;";
const SOURCE_DEBUG_EXTENSION_DESC: &str = "Lkotlin/jvm/internal/SourceDebugExtension;";
/// The package of the coroutine base classes (`isCoroutineSuperClass`).
const COROUTINE_BASE_PACKAGE: &str = "kotlin/coroutines/jvm/internal/";

/// Why an anonymous object could not be regenerated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RegenerationError {
    /// A shape the ported stages do not regenerate yet.
    Unsupported(&'static str),
    /// A member could not be written as it was.
    Copy(CopyError),
    /// The original's `@Metadata` could not be read.
    Metadata(MetadataDecodeError),
    /// A descriptor or signature of the original does not parse.
    Malformed(MalformedType),
    /// A lambda could not be inlined into a method of the copy.
    Inlining(Box<InlineError>),
    /// The stack a lambda was inlined over could not be saved around it.
    FixStack(crate::jvm::bytecode_passes::fix_stack::FixStackError),
}

impl From<MalformedType> for RegenerationError {
    fn from(malformed: MalformedType) -> Self {
        RegenerationError::Malformed(malformed)
    }
}

/// The method an object is regenerated for (`InlineCallSiteInfo`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallSite<'a> {
    pub owner: &'a str,
    pub method: &'a str,
    pub descriptor: &'a str,
    /// Whether the method is a public inline function, which makes the copy public ABI too
    /// (`isInPublicInlineScope`).
    pub public_inline_scope: bool,
}

/// One object to regenerate (`AnonymousObjectTransformationInfo`).
pub(crate) struct Regeneration<'a> {
    pub original: &'a ClassNode,
    pub new_class: &'a str,
    /// The descriptor the inline body calls the original's constructor through.
    pub constructor_desc: &'a str,
    pub call_site: CallSite<'a>,
    /// The inline function's type arguments at the call, each as `(parameter name, signature)`.
    pub type_arguments: &'a [(String, String)],
    /// Anonymous classes already regenerated earlier in the same inlined body. A later copy can
    /// capture or otherwise name one of them and must point at the call-site class.
    pub prior_classes: &'a [(String, String)],
    /// The call's reified arguments, which specialize `reifiedOperationMarker` in the copy.
    pub(in crate::jvm) reified: &'a crate::jvm::reified_arguments::ReifiedArguments,
    /// The caller's class-file major version.
    pub major: u16,
    /// The `@Metadata` version the caller writes.
    pub metadata_version: &'a [i32],
    /// The call's inline lambdas the constructor call passes, which the copy inlines.
    pub lambdas: &'a [ObjectLambda<'a>],
    /// The caller's source map, which the lambdas' lines are lines of.
    pub caller_lines: &'a SourceMap,
    /// Which classes the original names are regenerated in turn.
    pub classes: &'a dyn ClassRoles,
    /// The caller's value classes, which the copy's code boxes like the caller's.
    pub value_classes: &'a Rc<ValueClassDescriptors>,
}

/// The regenerated class.
#[derive(Debug)]
pub(crate) struct Regenerated {
    pub bytes: Vec<u8>,
    /// The regenerated constructor's descriptor, which the call site now calls.
    pub constructor_desc: String,
    /// A method of the copy still calls `reifiedOperationMarker`.
    pub reified_parameters_remain: bool,
}

/// `isCapturedFieldName`: a field an object's constructor fills from a captured value.
pub(super) fn is_captured_field_name(name: &str) -> bool {
    name.starts_with('$') && !name.starts_with("$$") && name != "$assertionsDisabled"
        || name == "this$0"
        || name == "receiver$0"
}

/// Copy `regeneration.original` as `regeneration.new_class`.
pub(crate) fn regenerate(
    regeneration: &Regeneration<'_>,
) -> Result<Regenerated, RegenerationError> {
    let original = regeneration.original;
    let old = original.name.as_str();
    let new = regeneration.new_class;
    check_supported(original, regeneration.classes)?;
    let mut remapper = TypeRemapper::with_type_arguments(regeneration.type_arguments);
    for (original, copy) in regeneration.prior_classes {
        remapper.add_mapping(original, copy);
    }
    remapper.add_mapping(old, new);

    let super_name = original
        .super_name
        .as_deref()
        .ok_or(RegenerationError::Unsupported(
            "a class without a superclass",
        ))?;
    let signature = original
        .signature
        .as_deref()
        .map(|s| remapper.map_signature(s))
        .transpose()?;
    let mut cw =
        ClassWriter::new_generic(new, signature.as_deref(), &remapper.map_type(super_name)?);
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    cw.set_value_classes(Rc::clone(regeneration.value_classes));
    cw.set_access(original.access);
    if original.deprecated {
        cw.set_deprecated();
    }
    cw.set_major(original.major.max(regeneration.major));
    for interface in &original.interfaces {
        cw.add_interface(&remapper.map_type(interface)?);
    }
    cw.keep_visited_inner_classes();
    // The original's own `EnclosingMethod`, passed through before the transformer replaces it.
    if let Some(outer) = &original.outer_class {
        cw.seed_class(&remapper.map_type(&outer.owner)?);
        if let Some((name, desc)) = &outer.method {
            cw.seed_name_and_type(name, &remapper.map_desc(desc)?);
        }
    }
    let mut metadata = None;
    for (annotations, visible) in [
        (&original.visible_annotations, true),
        (&original.invisible_annotations, false),
    ] {
        for annotation in annotations {
            match annotation.desc.as_str() {
                METADATA_DESC => metadata = Some(annotation),
                // The copy's map is written with the class; kotlinc drops the original's.
                SOURCE_DEBUG_EXTENSION_DESC => {}
                _ => {
                    let mut annotation = annotation.clone();
                    remapper.remap_annotation(&mut annotation)?;
                    cw.add_copied_class_annotation(&annotation, visible);
                }
            }
        }
    }
    for field in &original.fields {
        if is_captured_field_name(&field.name) {
            continue;
        }
        let mut field = field.clone();
        field.desc = remapper.map_desc(&field.desc)?;
        field.signature = field
            .signature
            .map(|s| remapper.map_signature(&s))
            .transpose()?;
        for annotation in field
            .visible_annotations
            .iter_mut()
            .chain(&mut field.invisible_annotations)
        {
            remapper.remap_annotation(annotation)?;
        }
        cw.add_copied_field(&field)
            .map_err(RegenerationError::Copy)?;
    }

    let mut lines = CopiedLines::of(original)?;
    let (constructor, methods) = split_constructor(original)?;
    let constructor_code = constructor
        .code
        .as_ref()
        .ok_or(RegenerationError::Unsupported("a constructor without code"))?;
    if constructor.desc != regeneration.constructor_desc {
        return Err(RegenerationError::Unsupported(
            "a constructor called through another descriptor",
        ));
    }
    let mut constructor_code = constructor_code.clone();
    let mut reified_parameters_remain =
        specialize_copied(&mut constructor_code, regeneration.reified)?;
    let declared: Vec<DeclaredCapture<'_>> = original
        .fields
        .iter()
        .filter(|field| is_captured_field_name(&field.name))
        .map(|field| DeclaredCapture {
            name: &field.name,
            desc: &field.desc,
        })
        .collect();
    let mut plan = constructor::extract(
        &constructor_code,
        old,
        &declared,
        regeneration.constructor_desc,
    )?;
    let lambdas = plan_lambdas(&mut plan, regeneration)?;
    plan.desc = remapper.map_desc(&plan.desc)?;
    for field in &mut plan.fields {
        field.desc = remapper.map_desc(&field.desc)?;
    }
    // `generateConstructorAndFields`: the constructor is declared, then each captured field, then
    // the constructor's body is written.
    cw.seed_utf8("<init>");
    cw.seed_utf8(&plan.desc);
    for field in &plan.fields {
        cw.add_copied_field(&FieldNode {
            access: CAPTURED_FIELD_ACCESS,
            name: field.name.clone(),
            desc: field.desc.clone(),
            signature: None,
            value: None,
            visible_annotations: Vec::new(),
            invisible_annotations: Vec::new(),
            deprecated: false,
        })
        .map_err(RegenerationError::Copy)?;
    }
    let body = copy_body(&plan.body, &remapper, &mut lines)?;
    let body = plan.assemble(new, body);
    cw.add_copied_method(&ClassMethod {
        access: constructor.access,
        name: "<init>".to_string(),
        desc: plan.desc.clone(),
        signature: None,
        exceptions: Vec::new(),
        visible_annotations: Vec::new(),
        invisible_annotations: Vec::new(),
        visible_parameter_annotations: None,
        invisible_parameter_annotations: None,
        annotation_default: None,
        deprecated: false,
        code: Some(body),
    })
    .map_err(RegenerationError::Copy)?;

    for method in methods {
        let mut method = method.clone();
        if let Some(code) = &mut method.code {
            reified_parameters_remain |= specialize_copied(code, regeneration.reified)?;
        }
        check_captured_field_accesses(&method, old, &declared)?;
        if let Some(code) = method
            .code
            .as_ref()
            .filter(|code| lambda_inlining::reads_lambda(code, &lambdas))
        {
            let mut code = code.clone();
            remap_declaration(&mut method, &mut remapper)?;
            remapper.remap_method(&mut code)?;
            method.code = Some(lambda_inlining::inline_into(
                &code,
                new,
                &lambdas,
                &mut lines,
                regeneration.caller_lines,
            )?);
            cw.add_copied_method(&method)
                .map_err(RegenerationError::Copy)?;
            continue;
        }
        remap_declaration(&mut method, &mut remapper)?;
        if let Some(code) = &method.code {
            method.code = Some(copy_body(code, &remapper, &mut lines)?);
        }
        cw.add_copied_method(&method)
            .map_err(RegenerationError::Copy)?;
    }

    cw.set_source_file(original.source_file.clone());
    cw.set_source_map(lines.map);
    for inner in &original.inner_classes {
        let spec = InnerClassSpec {
            inner: remapper.map_type(&inner.name)?,
            outer: inner
                .outer_name
                .as_deref()
                .map(|outer| remapper.map_type(outer))
                .transpose()?,
            name: inner
                .inner_name
                .as_deref()
                .map(|simple| remapper.map_inner_class_name(&inner.name, simple)),
            access: inner.access,
        };
        // `ClassWriter.visitInnerClass` interns the row as it is visited.
        cw.seed_class(&spec.inner);
        if let Some(outer) = &spec.outer {
            cw.seed_class(outer);
        }
        if let Some(name) = &spec.name {
            cw.seed_utf8(name);
        }
        cw.add_inner_class(spec);
    }
    if let Some(metadata) = metadata {
        write_metadata(&mut cw, metadata, old, regeneration)?;
    }
    let call_site = regeneration.call_site;
    cw.seed_class(call_site.owner);
    cw.seed_name_and_type(call_site.method, call_site.descriptor);
    cw.set_enclosing_method(call_site.owner, call_site.method, call_site.descriptor);
    cw.intern_source_and_map();
    Ok(Regenerated {
        bytes: cw.finish(),
        constructor_desc: plan.desc,
        reified_parameters_remain,
    })
}

/// Specialize `body`'s reified markers. `true` when a marker remains for an enclosing inline
/// function to specialize in turn.
fn specialize_copied(
    body: &mut MethodNode,
    reified: &crate::jvm::reified_arguments::ReifiedArguments,
) -> Result<bool, RegenerationError> {
    super::reified::specialize(body, reified)
        .map_err(|error| RegenerationError::Inlining(Box::new(error)))?;
    Ok(super::reified::has_reified_markers(body))
}

/// A method's descriptor, signature and annotations as the copy declares them.
fn remap_declaration(
    method: &mut ClassMethod,
    remapper: &mut TypeRemapper,
) -> Result<(), RegenerationError> {
    method.desc = remapper.map_desc(&method.desc)?;
    method.signature = method
        .signature
        .take()
        .map(|s| remapper.map_signature(&s))
        .transpose()?;
    for annotation in method
        .visible_annotations
        .iter_mut()
        .chain(&mut method.invisible_annotations)
        .chain(
            method
                .visible_parameter_annotations
                .iter_mut()
                .flatten()
                .flatten(),
        )
        .chain(
            method
                .invisible_parameter_annotations
                .iter_mut()
                .flatten()
                .flatten(),
        )
    {
        remapper.remap_annotation(annotation)?;
    }
    Ok(())
}

/// The lambdas the copy inlines, each with the original's field for it and the copy's fields for
/// its captured values, which `plan` now takes in the lambdas' place
/// (`extractParametersMappingAndPatchConstructor`). A captured value's field is named after it with
/// `$inlined` added, numbered when that name is taken (`getNewFieldName`, `addUniqueField`): every
/// field the original declares is taken first, as kotlinc's `visitField` records each one it
/// copies before the constructor is patched.
fn plan_lambdas<'a>(
    plan: &mut constructor::Constructor,
    regeneration: &Regeneration<'a>,
) -> Result<Vec<FieldLambda<'a>>, RegenerationError> {
    if regeneration.lambdas.is_empty() {
        return Ok(Vec::new());
    }
    let mut taken: HashMap<String, usize> = HashMap::new();
    for field in &regeneration.original.fields {
        *taken.entry(field.name.clone()).or_default() += 1;
    }
    let mut lambdas = Vec::with_capacity(regeneration.lambdas.len());
    for passed in regeneration.lambdas {
        let types = passed
            .lambda
            .captured_types()
            .ok_or(RegenerationError::Unsupported(
                "a malformed lambda descriptor",
            ))?;
        if types.len() != passed.lambda.capture_names.len() {
            return Err(RegenerationError::Unsupported(
                "a lambda whose captured values are not all named",
            ));
        }
        let mut captured = Vec::with_capacity(types.len());
        for (name, desc) in passed.lambda.capture_names.iter().zip(types) {
            let name = name.as_deref().ok_or(RegenerationError::Unsupported(
                "a lambda capturing a value the port does not name",
            ))?;
            let base = format!("{name}$inlined");
            let seen = taken.entry(base.clone()).or_default();
            let unique = match *seen {
                0 => base,
                count => format!("{base}${count}"),
            };
            *seen += 1;
            captured.push((unique, desc));
        }
        lambdas.push(FieldLambda {
            field: String::new(),
            lambda: passed.lambda,
            captured,
        });
    }
    let arguments: Vec<usize> = regeneration
        .lambdas
        .iter()
        .map(|passed| passed.argument)
        .collect();
    let recaptured: Vec<(String, String)> = lambdas
        .iter()
        .flat_map(|lambda| lambda.captured.iter().cloned())
        .collect();
    let fields = plan.inline_lambdas(regeneration.constructor_desc, &arguments, &recaptured)?;
    for (lambda, field) in lambdas.iter_mut().zip(fields) {
        lambda.field = field;
    }
    Ok(lambdas)
}

/// Refuse what the ported stages do not regenerate yet.
fn check_supported(
    original: &ClassNode,
    classes: &dyn ClassRoles,
) -> Result<(), RegenerationError> {
    if original
        .super_name
        .as_deref()
        .is_some_and(|name| name.starts_with(COROUTINE_BASE_PACKAGE))
    {
        return Err(RegenerationError::Unsupported("a coroutine"));
    }
    if original
        .visible_annotations
        .iter()
        .any(|annotation| annotation.desc == DEBUG_METADATA_DESC)
    {
        return Err(RegenerationError::Unsupported("coroutine debug metadata"));
    }
    if original.source_file.is_none() {
        return Err(RegenerationError::Unsupported(
            "a class without a source file",
        ));
    }
    for method in &original.methods {
        let Some(code) = &method.code else { continue };
        // A declaration lambda whose function type mentions a reified parameter guards its own
        // singleton initialization. Copying that initializer verbatim leaves a throwing
        // `needClassReification` in the call-site class even after its `invoke` method is
        // specialized. Until initializer specialization owns that marker, fail the inline call
        // instead of emitting a class that throws when its INSTANCE is first read.
        if method.name == "<clinit>"
            && code.instructions().any(|insn| {
                matches!(insn, Insn::Method {
                    op: INVOKESTATIC,
                    owner,
                    name,
                    desc,
                    interface: false,
                } if owner == INTRINSICS
                    && name == "needClassReification"
                    && desc == "()V")
            })
        {
            return Err(RegenerationError::Unsupported(
                "a class initializer requiring reification",
            ));
        }
        if code
            .instructions()
            .any(|insn| references_other_regenerated_class(insn, classes, &original.name))
        {
            return Err(RegenerationError::Unsupported("a nested anonymous object"));
        }
    }
    Ok(())
}

/// Whether `insn` constructs or loads an object other than `self_name` that a copy would
/// regenerate in turn. The class's own `<clinit>` constructs itself; that is the singleton, not a
/// nested object.
fn references_other_regenerated_class(
    insn: &Insn,
    classes: &dyn ClassRoles,
    self_name: &str,
) -> bool {
    let other = |name: &str| name != self_name && classes.is_regenerated(name);
    match insn {
        Insn::Type { op: NEW, class } => other(class),
        Insn::Method {
            op: INVOKESPECIAL,
            owner,
            name,
            ..
        } => name == "<init>" && other(owner),
        Insn::Field {
            op: GETSTATIC,
            owner,
            name,
            ..
        } => name == "INSTANCE" && other(owner),
        _ => false,
    }
}

/// The original's one constructor and its other methods, in order.
fn split_constructor(
    original: &ClassNode,
) -> Result<(&ClassMethod, Vec<&ClassMethod>), RegenerationError> {
    let mut constructors = original.methods.iter().filter(|m| m.name == "<init>");
    let constructor = constructors.next().ok_or(RegenerationError::Unsupported(
        "a class without a constructor",
    ))?;
    if constructors.next().is_some() {
        return Err(RegenerationError::Unsupported("more than one constructor"));
    }
    let methods = original
        .methods
        .iter()
        .filter(|m| m.name != "<init>")
        .collect();
    Ok((constructor, methods))
}

/// Outside the constructor a captured field is read as `aload 0; getfield` of the original, which
/// kotlinc's field remapper folds and unfolds back to the same read of the copy. Any other access
/// (through an alias of `this`, an outer object, or a store) is remapped differently, which this
/// stage does not port.
fn check_captured_field_accesses(
    method: &ClassMethod,
    old: &str,
    declared: &[DeclaredCapture<'_>],
) -> Result<(), RegenerationError> {
    let Some(code) = &method.code else {
        return Ok(());
    };
    let mut previous: Option<&Insn> = None;
    for entry in &code.nodes {
        let Node::Insn(insn) = entry else {
            previous = None;
            continue;
        };
        if let Insn::Field {
            op,
            owner,
            name,
            desc,
        } = insn
        {
            if (*op == GETFIELD || *op == PUTFIELD) && is_captured_field_name(name) {
                let direct = *op == GETFIELD
                    && owner == old
                    && declared
                        .iter()
                        .any(|field| field.name == name && field.desc == desc)
                    && matches!(previous, Some(Insn::Var { op: ALOAD, slot: 0 }));
                if !direct {
                    return Err(RegenerationError::Unsupported(
                        "a captured field accessed other than from this",
                    ));
                }
            }
        }
        previous = Some(insn);
    }
    Ok(())
}

/// A body of the original as the copy carries it: every regenerated class renamed and every line
/// mapped through the copy's source map (`MethodInliner` over a `RegeneratedLambdaFieldRemapper`).
fn copy_body(
    body: &MethodNode,
    remapper: &TypeRemapper,
    lines: &mut CopiedLines,
) -> Result<MethodNode, RegenerationError> {
    let mut body = body.clone();
    remapper.remap_method(&mut body)?;
    // One `SourceMapCopier` per method: a line maps once and keeps that mapping.
    let mut mapped: HashMap<u16, u16> = HashMap::new();
    for entry in &mut body.nodes {
        if let Node::Line { line, .. } = entry {
            *line = match mapped.get(line) {
                Some(&known) => known,
                None => {
                    let to = lines.map(*line)?;
                    mapped.insert(*line, to);
                    to
                }
            };
        }
    }
    Ok(body)
}

/// `SourceMapper(debugFileName, originalSmap)` and the original map it copies lines out of.
struct CopiedLines {
    map: SourceMap,
    original: Option<DependencyMap>,
    source_file: String,
    path: String,
}

impl CopiedLines {
    fn of(original: &ClassNode) -> Result<CopiedLines, RegenerationError> {
        let source_file = original
            .source_file
            .clone()
            .ok_or(RegenerationError::Unsupported(
                "a class without a source file",
            ))?;
        let (map, parsed) = match &original.source_debug {
            Some(text) => {
                let parsed = DependencyMap::parse(text)
                    .ok_or(RegenerationError::Unsupported("an unreadable source map"))?;
                let (path, lines) =
                    parsed
                        .source_info(&source_file)
                        .ok_or(RegenerationError::Unsupported(
                            "a source map without its own file",
                        ))?;
                (
                    SourceMap::for_regenerated_class(&source_file, path, lines),
                    Some(parsed),
                )
            }
            // `SMAP.identityMapping`: the class's own file, as far as its highest line.
            None => {
                let lines = original
                    .methods
                    .iter()
                    .filter_map(|method| method.code.as_ref())
                    .flat_map(|code| &code.nodes)
                    .filter_map(|entry| match entry {
                        Node::Line { line, .. } => Some(*line),
                        _ => None,
                    })
                    .max()
                    .ok_or(RegenerationError::Unsupported(
                        "a class without line numbers",
                    ))?;
                (
                    SourceMap::for_regenerated_class(&source_file, &original.name, lines),
                    None,
                )
            }
        };
        Ok(CopiedLines {
            map,
            original: parsed,
            path: original.name.clone(),
            source_file,
        })
    }

    /// `SourceMapCopier.mapLineNumber`.
    fn map(&mut self, line: u16) -> Result<u16, RegenerationError> {
        let unmapped = RegenerationError::Unsupported("a line the source map does not cover");
        let mapped = match &self.original {
            Some(original) => {
                let (name, path, source) = original.resolve(line).ok_or(unmapped.clone())?;
                let call_line = original.call_site(line).map(|(call_line, ..)| call_line);
                let (name, path) = (name.to_string(), path.to_string());
                self.map.map_copied_line(&name, &path, source, call_line)
            }
            None => self
                .map
                .map_copied_line(&self.source_file, &self.path, line, None),
        };
        mapped.ok_or(unmapped)
    }
}

/// `writeTransformedMetadata`: the original's kind and payload with the origin recorded, under the
/// caller's metadata version, public ABI only when the call is in a public inline function.
fn write_metadata(
    cw: &mut ClassWriter,
    metadata: &Annotation,
    old: &str,
    regeneration: &Regeneration<'_>,
) -> Result<(), RegenerationError> {
    let malformed = RegenerationError::Unsupported("unreadable @Metadata");
    let kind = metadata
        .value("k")
        .and_then(ElementValue::as_int)
        .ok_or(malformed.clone())?;
    let extra = metadata
        .value("xi")
        .and_then(ElementValue::as_int)
        .unwrap_or(0);
    if extra & METADATA_PUBLIC_ABI_FLAG == 0 {
        return Err(RegenerationError::Unsupported(
            "an object outside the public ABI",
        ));
    }
    let strings = |name| {
        metadata
            .value(name)
            .map(|value| value.as_strings().ok_or(malformed.clone()))
            .transpose()
    };
    // A synthetic class can carry only `k`/`mv`/`xi` (`filterIsInstance`'s singleton). kotlinc
    // still writes that header on the copy and records an origin only when `d1`/`d2` are present.
    let (d1, d2) = match (strings("d1")?, strings("d2")?) {
        (Some(d1), Some(d2)) => {
            let original = MetadataStrings { d1, d2 };
            let copy = record_origin_name(kind, &original, old)
                .map_err(RegenerationError::Metadata)?
                .unwrap_or(original);
            (copy.d1, copy.d2)
        }
        (None, None) => (Vec::new(), Vec::new()),
        _ => return Err(malformed),
    };
    let mut flags = extra & !METADATA_PUBLIC_ABI_FLAG;
    if regeneration.call_site.public_inline_scope {
        flags |= METADATA_PUBLIC_ABI_FLAG;
    }
    if flags == 0 {
        return Err(RegenerationError::Unsupported(
            "@Metadata without extra flags",
        ));
    }
    cw.set_kotlin_metadata(kind, regeneration.metadata_version, flags, &d1, &d2);
    Ok(())
}
