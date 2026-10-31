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
mod nested_objects;
use nested_objects::NestedObjects;
mod sam_wrapper;
mod suspend_methods;
mod type_remapper;

use crate::fx_hash::HashMap;
use std::rc::Rc;

use crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors;
use crate::jvm::class_node::{Annotation, ClassMethod, ClassNode, ElementValue, FieldNode};
use crate::jvm::classfile::{ClassWriter, CopyError, InnerClassSpec};
use crate::jvm::metadata::anonymous_origin::{record_origin_name, MetadataStrings};
use crate::jvm::metadata::MetadataDecodeError;
use crate::jvm::method_node::{Insn, MethodNode, Node};
use crate::jvm::source_map::{DependencyMap, SourceMap};

use super::class_roles::ClassRoles;
use super::name_generator::NameGenerator;
use super::{InlineError, ObjectLambda};

pub(crate) use sam_wrapper::copy_sam_wrapper;
pub(crate) use suspend_methods::CallSiteSource;
use suspend_methods::ContinuationMachine;
pub(crate) use type_remapper::{MalformedType, TypeRemapper};

const GETSTATIC: u8 = 0xb2;
const INVOKESPECIAL: u8 = 0xb7;
const NEW: u8 = 0xbb;

/// `ACC_SYNTHETIC | ACC_FINAL`, a regenerated captured field's access
/// (`NO_FLAG_PACKAGE_PRIVATE | ACC_SYNTHETIC | ACC_FINAL`).
const CAPTURED_FIELD_ACCESS: u16 = 0x1010;
/// `JvmAnnotationNames.METADATA_PUBLIC_ABI_FLAG`.
const METADATA_PUBLIC_ABI_FLAG: i32 = 1 << 7;
const METADATA_DESC: &str = "Lkotlin/Metadata;";
const DEBUG_METADATA_DESC: &str = "Lkotlin/coroutines/jvm/internal/DebugMetadata;";
const SUSPEND_LAMBDA: &str = "kotlin/coroutines/jvm/internal/SuspendLambda";
const SOURCE_DEBUG_EXTENSION_DESC: &str = "Lkotlin/jvm/internal/SourceDebugExtension;";
/// The package of the coroutine base classes (`isCoroutineSuperClass`).
const COROUTINE_BASE_PACKAGE: &str = "kotlin/coroutines/jvm/internal/";
const CONTINUATION_IMPL: &str = "kotlin/coroutines/jvm/internal/ContinuationImpl";

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
    /// The coroutine transformer could not build a suspend method of the copy.
    Coroutine(crate::jvm::bytecode_passes::coroutines::CoroutineError),
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
    /// Where the call is, which the copy's suspend methods report.
    pub source: CallSiteSource<'a>,
}

/// One object to regenerate (`AnonymousObjectTransformationInfo`).
pub(crate) struct Regeneration<'a> {
    pub original: &'a ClassNode,
    pub new_class: &'a str,
    /// The descriptor the inline body calls the original's constructor through.
    pub constructor_desc: &'a str,
    pub call_site: CallSite<'a>,
    /// The class the inline call is written in, which owns the copies of the SAM wrappers the
    /// copy's methods construct.
    pub calling_class: &'a str,
    /// The inline function's type arguments at the call, each as `(parameter name, signature)`.
    pub type_arguments: &'a [(String, String)],
    /// Anonymous classes already regenerated earlier in the same inlined body. A later copy can
    /// capture or otherwise name one of them and must point at the call-site class.
    pub prior_classes: &'a [(String, String)],
    /// The call's reified arguments, which specialize `reifiedOperationMarker` in the copy.
    pub(in crate::jvm) reified: &'a crate::jvm::reified_arguments::ReifiedArguments,
    /// The caller's class-file major version.
    pub major: u16,
    /// The `@Metadata` stamp the caller writes.
    pub metadata_stamp: crate::jvm::classfile::MetadataStamp,
    /// The call's inline lambdas the constructor call passes, which the copy inlines.
    pub lambdas: &'a [ObjectLambda<'a>],
    /// The caller's source map, which the lambdas' lines are lines of.
    pub caller_lines: &'a SourceMap,
    /// Which classes the original names are regenerated in turn.
    pub classes: &'a dyn ClassRoles,
    /// The caller's value classes, which the copy's code boxes like the caller's.
    pub value_classes: &'a Rc<ValueClassDescriptors>,
    /// The state machine whose continuation this copy is, when it is the continuation of a copied
    /// suspend method (`isContinuation`).
    pub continuation: Option<&'a ContinuationMachine>,
    /// The inlined callee is the call site's module's, whose nested objects a copy reuses unless
    /// it must specialize them (kotlinc's `sameModule`).
    pub same_module: bool,
}

/// The regenerated class.
#[derive(Debug)]
pub(crate) struct Regenerated {
    pub bytes: Vec<u8>,
    /// The regenerated constructor's descriptor, which the call site now calls.
    pub constructor_desc: String,
    /// A method of the copy still calls `reifiedOperationMarker`.
    pub reified_parameters_remain: bool,
    /// The classes the copy regenerated in turn: the continuations of its suspend methods.
    pub nested: Vec<(String, Vec<u8>)>,
    /// The SAM wrappers the copy's methods construct, copied into the calling class. The calling
    /// class shares one copy of each among its inline calls, so the call site writes only the
    /// first.
    pub sam_wrappers: Vec<SamWrapperCopy>,
}

/// A SAM wrapper copied into the calling class.
#[derive(Debug)]
pub(crate) struct SamWrapperCopy {
    /// The wrapper the inline body constructs.
    pub original: String,
    pub name: String,
    pub bytes: Vec<u8>,
}

/// `isCapturedFieldName`: a field an object's constructor fills from a captured value.
pub(super) fn is_captured_field_name(name: &str) -> bool {
    name.starts_with('$') && !name.starts_with("$$") && name != "$assertionsDisabled"
        || name == "this$0"
        || name == "receiver$0"
}

/// Copy `regeneration.original` as `regeneration.new_class`, whose own regenerated classes are
/// named by `names`.
pub(crate) fn regenerate(
    regeneration: &Regeneration<'_>,
    names: &mut NameGenerator,
) -> Result<Regenerated, RegenerationError> {
    let original = regeneration.original;
    let old = original.name.as_str();
    let new = regeneration.new_class;
    let (constructor, methods) = split_constructor(original)?;
    let for_inline = suspend_methods::ForInlineCopies::of(&methods);
    // The continuation each `$$forInline` copy builds, which the copy names in place of a nested
    // object of its own.
    let mut continuations = Vec::new();
    for method in &methods {
        if let (Some(_), Some(code)) = (
            suspend_methods::for_inline_original(&method.name),
            &method.code,
        ) {
            let continuation = suspend_methods::fake_continuation_class(code)?.ok_or(
                RegenerationError::Unsupported("a $$forInline method without a continuation"),
            )?;
            continuations.push(continuation.to_string());
        }
    }
    check_supported(
        original,
        regeneration.continuation.is_some(),
        regeneration.lambdas.is_empty(),
    )?;
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
                // So is a continuation's `@DebugMetadata`: its machine's, which a copy built for
                // a suspend method writes anew, and which a copied continuation does not keep.
                DEBUG_METADATA_DESC
                    if regeneration.continuation.is_some()
                        || is_continuation_not_lambda(original) => {}
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
    let constructor_code = constructor
        .code
        .as_ref()
        .ok_or(RegenerationError::Unsupported("a constructor without code"))?;
    // An object a copy's method constructs is called with the copy's types already in place: one
    // that captures the copy as its outer instance passes the copy where the original took its
    // original. The constructor is planned from the original's own descriptor, then renamed.
    if constructor.desc != regeneration.constructor_desc
        && remapper.map_desc(&constructor.desc)? != regeneration.constructor_desc
    {
        return Err(RegenerationError::Unsupported(
            "a constructor called through another descriptor",
        ));
    }
    let call_desc = constructor.desc.as_str();
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
    let mut plan = constructor::extract(&constructor_code, old, &declared, call_desc)?;
    let lambdas = plan_lambdas(&mut plan, call_desc, regeneration)?;
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
    // kotlinc's `MethodInliner` transforms the constructor first: the objects it constructs are
    // the copy's first ones, enclosed by the new constructor. What is left of its body reads no
    // lambda, so only those objects are regenerated.
    let mut nested = Vec::new();
    let mut sam_wrappers = Vec::new();
    let regenerates_objects = plan.body.instructions().any(|insn| {
        references_other_regenerated_class(insn, regeneration.classes, old, &continuations)
    });
    let body = if regenerates_objects {
        let mut body = plan.body.clone();
        body.desc.clone_from(&plan.desc);
        let mut objects = NestedObjects {
            regeneration,
            names: &mut *names,
            method: "<init>",
            descriptor: &plan.desc,
            renamed: Vec::new(),
            classes: Vec::new(),
            sam_wrappers: Vec::new(),
            reified_parameters_remain: false,
        };
        let body = transform_method(body, new, &[], &mut remapper, &mut lines, &mut objects)?;
        nested.extend(objects.classes);
        sam_wrappers.extend(objects.sam_wrappers);
        reified_parameters_remain |= objects.reified_parameters_remain;
        body
    } else {
        copy_body(&plan.body, &remapper, &mut lines)?
    };
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
        parameters: Vec::new(),
        deprecated: false,
        code: Some(body),
    })
    .map_err(RegenerationError::Copy)?;

    // The continuations of copies that need none, whose `InnerClasses` rows go with them.
    let mut dropped_continuations = Vec::new();
    for method in methods {
        if for_inline.skips(method) {
            continue;
        }
        let mut method = method.clone();
        if let Some(code) = &mut method.code {
            reified_parameters_remain |= specialize_copied(code, regeneration.reified)?;
            if method.name == "<clinit>" {
                // kotlinc's copy of a class whose own type names a reified parameter initializes
                // its singleton unguarded: the site that loads it carries the marker when a
                // parameter remains for an enclosing inline function.
                code.nodes.retain(|node| {
                    !matches!(node, Node::Insn(insn) if super::object_regeneration::is_need_class_reification(insn))
                });
            }
        }
        check_captured_field_accesses(&method, old, &declared)?;
        // The continuation a `$$forInline` copy builds is regenerated as the copy's next object
        // when the inliner meets its construction.
        let continuation = match (
            suspend_methods::for_inline_original(&method.name),
            &method.code,
        ) {
            (Some(_), Some(code)) => {
                let original = suspend_methods::fake_continuation_class(code)?
                    .ok_or(RegenerationError::Unsupported(
                        "a $$forInline method without a continuation",
                    ))?
                    .to_string();
                let copy = names.next_object().class().to_string();
                remapper.add_mapping(&original, &copy);
                Some((original, copy))
            }
            _ => None,
        };
        remap_declaration(&mut method, &mut remapper)?;
        if let Some(code) = &method.code {
            // Only a `$$forInline` copy's machine takes the suspension points a suspend lambda
            // brings: kotlinc writes one for every suspend method that invokes such a lambda.
            if continuation.is_none() && lambda_inlining::reads_suspend_lambda(code, &lambdas) {
                return Err(RegenerationError::Unsupported(
                    "a suspend lambda in a method without a $$forInline copy",
                ));
            }
            // kotlinc's `MethodInliner` transforms every method of a copy: one that reads an inlined
            // lambda, or names an object the copy regenerates in turn (a reified lambda nested in
            // it), has its objects regenerated as the copy's next ones.
            let regenerates_objects = lambda_inlining::reads_lambda(code, &lambdas)
                || code.instructions().any(|insn| {
                    references_other_regenerated_class(
                        insn,
                        regeneration.classes,
                        old,
                        &continuations,
                    )
                });
            let code = if regenerates_objects {
                let mut objects = NestedObjects {
                    regeneration,
                    names: &mut *names,
                    method: suspend_methods::for_inline_original(&method.name)
                        .unwrap_or(&method.name),
                    descriptor: &method.desc,
                    renamed: Vec::new(),
                    classes: Vec::new(),
                    sam_wrappers: Vec::new(),
                    reified_parameters_remain: false,
                };
                let code = transform_method(
                    code.clone(),
                    new,
                    &lambdas,
                    &mut remapper,
                    &mut lines,
                    &mut objects,
                )?;
                nested.extend(objects.classes);
                sam_wrappers.extend(objects.sam_wrappers);
                reified_parameters_remain |= objects.reified_parameters_remain;
                code
            } else {
                copy_body(code, &remapper, &mut lines)?
            };
            method.code = Some(code);
        }
        let Some((original_continuation, continuation)) = continuation else {
            cw.add_copied_method(&method)
                .map_err(RegenerationError::Copy)?;
            continue;
        };
        // `newStateMachineForNamedFunction`: the copy is the method itself, its state machine built
        // over the inlined body.
        let name = suspend_methods::for_inline_original(&method.name)
            .unwrap_or_default()
            .to_string();
        let mut body = method
            .code
            .take()
            .unwrap_or_else(|| unreachable!("a $$forInline copy with a continuation has code"));
        body.name.clone_from(&name);
        method.name = name;
        let (body, machine) =
            suspend_methods::transform(body, new, &continuation, &regeneration.call_site.source)?;
        method.code = Some(body);
        cw.add_copied_transformed_method(&method)
            .map_err(RegenerationError::Copy)?;
        let Some(machine) = machine else {
            dropped_continuations.push(original_continuation);
            continue;
        };
        // The continuation is regenerated as a class of the copy's, enclosed by the method its
        // machine belongs to.
        let bytes = regeneration
            .classes
            .class_file(&original_continuation)
            .ok_or(RegenerationError::Unsupported(
                "a continuation whose class file is not on the classpath",
            ))?;
        let original_continuation = ClassNode::read(&bytes)
            .map_err(|_| RegenerationError::Unsupported("a continuation that does not read"))?;
        let constructor_desc = split_constructor(&original_continuation)?.0.desc.clone();
        let mut prior_classes = regeneration.prior_classes.to_vec();
        prior_classes.push((old.to_string(), new.to_string()));
        let regenerated = regenerate(
            &Regeneration {
                original: &original_continuation,
                new_class: &continuation,
                constructor_desc: &constructor_desc,
                call_site: CallSite {
                    owner: new,
                    method: &method.name,
                    descriptor: &method.desc,
                    public_inline_scope: false,
                    source: regeneration.call_site.source,
                },
                prior_classes: &prior_classes,
                lambdas: &[],
                continuation: Some(&machine),
                ..*regeneration
            },
            names
                .object(&continuation)
                .unwrap_or_else(|| unreachable!("the continuation's name was taken")),
        )?;
        nested.extend(regenerated.nested);
        sam_wrappers.extend(regenerated.sam_wrappers);
        nested.push((continuation, regenerated.bytes));
    }

    if !lines.lineless {
        cw.set_source_file(original.source_file.clone());
        cw.set_source_map(lines.map);
    }
    for inner in &original.inner_classes {
        if dropped_continuations.contains(&inner.name) {
            continue;
        }
        // The copy's code names a SAM wrapper's copy, but kotlinc rewrites only the `new` for it,
        // so the class's own `InnerClasses` row still names the original wrapper.
        let copied_wrapper = sam_wrappers
            .iter()
            .any(|wrapper: &SamWrapperCopy| wrapper.original == inner.name);
        let spec = InnerClassSpec {
            inner: if copied_wrapper {
                inner.name.clone()
            } else {
                remapper.map_type(&inner.name)?
            },
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
    // The machine's spill fields and `@DebugMetadata`, which the transformer adds to the
    // continuation once it built the machine. A spill field the original's own machine declared
    // is already copied.
    if let Some(machine) = regeneration.continuation {
        for field in &machine.fields {
            let copied = original
                .fields
                .iter()
                .any(|own| own.name == field.name && own.desc == field.descriptor);
            if !copied {
                cw.add_field_sig(0, &field.name, &field.descriptor, None);
            }
        }
        cw.set_debug_metadata(&machine.debug_metadata);
    }
    cw.intern_source_and_map();
    Ok(Regenerated {
        bytes: cw.finish(),
        constructor_desc: plan.desc,
        reified_parameters_remain,
        nested,
        sam_wrappers,
    })
}

/// Specialize `body`'s reified markers. `true` when a marker remains for an enclosing inline
/// function to specialize in turn.
fn specialize_copied(
    body: &mut MethodNode,
    reified: &crate::jvm::reified_arguments::ReifiedArguments,
) -> Result<bool, RegenerationError> {
    super::reified::specialize(
        body,
        reified,
        &mut crate::jvm::reified_arguments::NoEnumEntriesReads,
    )
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
    call_desc: &str,
    regeneration: &Regeneration<'a>,
) -> Result<Vec<FieldLambda<'a>>, RegenerationError> {
    if regeneration.lambdas.is_empty() {
        return Ok(Vec::new());
    }
    let mut taken: HashMap<String, usize> = HashMap::default();
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
    let fields = plan.inline_lambdas(call_desc, &arguments, &recaptured)?;
    for (lambda, field) in lambdas.iter_mut().zip(fields) {
        lambda.field = field;
    }
    Ok(lambdas)
}

/// Refuse what the ported stages do not regenerate yet.
fn check_supported(
    original: &ClassNode,
    continuation: bool,
    specialization_only: bool,
) -> Result<(), RegenerationError> {
    // A continuation copied for a suspend method's machine is one coroutine the port regenerates.
    // A suspend lambda the call passes no lambda to is another: its copy only specializes its
    // reified operations, so its machine and `@DebugMetadata` stay as they are. The continuation of
    // a suspend method a regenerated object declares is the third: kotlinc never builds a machine
    // for a continuation (`isContinuationNotLambda`), so it is copied like any object, without
    // its `@DebugMetadata`.
    let regenerated_coroutine = (continuation
        && original.super_name.as_deref() == Some(CONTINUATION_IMPL))
        || (specialization_only
            && !continuation
            && original.super_name.as_deref() == Some(SUSPEND_LAMBDA))
        || (specialization_only && is_continuation_not_lambda(original));
    if !regenerated_coroutine
        && original
            .super_name
            .as_deref()
            .is_some_and(|name| name.starts_with(COROUTINE_BASE_PACKAGE))
    {
        return Err(RegenerationError::Unsupported("a coroutine"));
    }
    // A copied continuation's `@DebugMetadata` is the copy's machine's, written anew.
    if !regenerated_coroutine
        && original
            .visible_annotations
            .iter()
            .any(|annotation| annotation.desc == DEBUG_METADATA_DESC)
    {
        return Err(RegenerationError::Unsupported("coroutine debug metadata"));
    }
    if original.source_file.is_none() && !regenerated_coroutine {
        return Err(RegenerationError::Unsupported(
            "a class without a source file",
        ));
    }
    Ok(())
}

/// kotlinc's `isContinuationNotLambda`: a continuation class, whose state machine is its suspend
/// method's, not its own.
fn is_continuation_not_lambda(original: &ClassNode) -> bool {
    original.super_name.as_deref().is_some_and(|name| {
        name.strip_prefix(COROUTINE_BASE_PACKAGE)
            .is_some_and(|name| name.ends_with("ContinuationImpl"))
    })
}

/// Whether `insn` constructs or loads an object other than `self_name` that a copy would
/// regenerate in turn. The class's own `<clinit>` constructs itself; that is the singleton, not a
/// nested object. Neither is the continuation of a suspend method, which its copy builds anew.
fn references_other_regenerated_class(
    insn: &Insn,
    classes: &dyn ClassRoles,
    self_name: &str,
    continuations: &[String],
) -> bool {
    let other = |name: &str| {
        name != self_name
            && !continuations
                .iter()
                .any(|continuation| continuation == name)
            && classes.is_regenerated(name)
    };
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

/// `code`, a method of the copy `new` with its types not yet renamed, as kotlinc's `MethodInliner`
/// transforms it: every `invoke` of one of `lambdas` becomes the lambda's body, and each object it
/// constructs that the copy regenerates in turn is regenerated into `objects`, whose originals the
/// method and the copy's later methods then name by their copies.
fn transform_method(
    mut code: MethodNode,
    new: &str,
    lambdas: &[FieldLambda<'_>],
    remapper: &mut TypeRemapper,
    lines: &mut CopiedLines,
    objects: &mut NestedObjects<'_, '_>,
) -> Result<MethodNode, RegenerationError> {
    remapper.remap_method(&mut code)?;
    let caller_lines = objects.regeneration.caller_lines;
    let mut code = lambda_inlining::inline_into(&code, new, lambdas, lines, caller_lines, objects)?;
    for (original, copy) in &objects.renamed {
        remapper.add_mapping(original, copy);
    }
    // kotlinc writes a copy's methods through its remapping class builder once every one is
    // transformed, so a reference before an object's `new` (a suspend method's `instanceof` of its
    // continuation) names the copy too.
    remapper.remap_method(&mut code)?;
    Ok(code)
}

/// The original's one constructor and its other methods, in order.
/// Whether `original`, constructed through `call_desc` with one of the call's suspend lambdas as
/// each argument at `suspend_arguments`, reads one of them from a method that has no `$$forInline`
/// copy. kotlinc writes that copy of every method of an object in an inline function's scope that
/// invokes a suspend inline lambda, and a regenerated object's state machine is built over it; a
/// body written without one cannot be regenerated.
pub(crate) fn reads_suspend_lambda_without_for_inline_copy(
    original: &ClassNode,
    call_desc: &str,
    suspend_arguments: &[usize],
) -> Result<bool, RegenerationError> {
    let (constructor, methods) = split_constructor(original)?;
    let code = constructor
        .code
        .as_ref()
        .ok_or(RegenerationError::Unsupported("a constructor without code"))?;
    let declared: Vec<DeclaredCapture<'_>> = original
        .fields
        .iter()
        .filter(|field| is_captured_field_name(&field.name))
        .map(|field| DeclaredCapture {
            name: &field.name,
            desc: &field.desc,
        })
        .collect();
    let plan = constructor::extract(code, &original.name, &declared, call_desc)?;
    let slots = constructor::argument_slots(call_desc).ok_or(RegenerationError::Unsupported(
        "a malformed constructor descriptor",
    ))?;
    let fields: Vec<&str> = plan
        .fields
        .iter()
        .filter(|field| {
            suspend_arguments
                .iter()
                .any(|&argument| slots.get(argument) == Some(&field.slot))
        })
        .map(|field| field.name.as_str())
        .collect();
    let for_inline = suspend_methods::ForInlineCopies::of(&methods);
    Ok(methods.iter().any(|method| {
        suspend_methods::for_inline_original(&method.name).is_none()
            && !for_inline.skips(method)
            && method.code.as_ref().is_some_and(|code| {
                code.instructions().any(|insn| {
                    matches!(insn, Insn::Field { owner, name, .. }
                        if *owner == original.name && fields.contains(&name.as_str()))
                })
            })
    }))
}

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
    match &method.code {
        Some(code) if !constructor::reads_captured_fields_from_this(code, old, declared) => Err(
            RegenerationError::Unsupported("a captured field accessed other than from this"),
        ),
        _ => Ok(()),
    }
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
    let mut mapped: HashMap<u16, u16> = HashMap::default();
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
    /// The original has no line at all, nor a map: kotlinc's mapper then maps nothing, and the
    /// copy carries neither a `SourceFile` nor a map (a continuation's, which only forwards).
    lineless: bool,
}

impl CopiedLines {
    fn of(original: &ClassNode) -> Result<CopiedLines, RegenerationError> {
        let lineless = || CopiedLines {
            map: SourceMap::default(),
            original: None,
            path: original.name.clone(),
            source_file: String::new(),
            lineless: true,
        };
        let has_lines = || {
            original
                .methods
                .iter()
                .filter_map(|method| method.code.as_ref())
                .flat_map(|code| &code.nodes)
                .any(|entry| matches!(entry, Node::Line { .. }))
        };
        // A continuation kotlinc writes without a source file has no lines either.
        let Some(source_file) = original.source_file.clone() else {
            if original.source_debug.is_none() && !has_lines() {
                return Ok(lineless());
            }
            return Err(RegenerationError::Unsupported(
                "a class without a source file",
            ));
        };
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
                let Some(lines) = original
                    .methods
                    .iter()
                    .filter_map(|method| method.code.as_ref())
                    .flat_map(|code| &code.nodes)
                    .filter_map(|entry| match entry {
                        Node::Line { line, .. } => Some(*line),
                        _ => None,
                    })
                    .max()
                else {
                    return Ok(CopiedLines {
                        source_file,
                        ..lineless()
                    });
                };
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
            lineless: false,
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
    cw.set_kotlin_metadata(kind, regeneration.metadata_stamp, flags, &d1, &d2);
    Ok(())
}
