//! The copy kotlinc's inliner makes of a SAM wrapper an inline function's body constructs.
//!
//! A wrapper declared in another module (`LibKt$sam$i$java_util_function_Supplier$0`) is copied
//! into the calling class as `<caller>$inlined$sam$i$…`, once per calling class. Unlike an
//! anonymous object, nothing in it is regenerated: the class is replayed as it was, with only its
//! own name remapped. Its `@Metadata`, its `EnclosingMethod` (the original's file class, which may
//! name no method), its source file and its member order are the original's, and its constant pool
//! follows the order a class reader replays them in: source, outer class, annotations, inner
//! classes, fields, methods.

use crate::jvm::class_node::ClassNode;
use crate::jvm::classfile::{ClassWriter, InnerClassSpec};

use super::{remap_declaration, RegenerationError, TypeRemapper};

/// Copy the SAM wrapper `original` as `new_class`.
pub(crate) fn copy_sam_wrapper(
    original: &ClassNode,
    new_class: &str,
) -> Result<Vec<u8>, RegenerationError> {
    if original.source_debug.is_some() {
        return Err(RegenerationError::Unsupported(
            "a SAM wrapper with a source map",
        ));
    }
    let mut remapper = TypeRemapper::with_type_arguments(&[]);
    remapper.add_mapping(&original.name, new_class);
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
    let mut cw = ClassWriter::new_generic(
        new_class,
        signature.as_deref(),
        &remapper.map_type(super_name)?,
    );
    if let Some(signature) = &signature {
        cw.set_signature(signature);
    }
    cw.set_access(original.access);
    cw.set_major(original.major);
    for interface in &original.interfaces {
        cw.add_interface(&remapper.map_type(interface)?);
    }
    cw.keep_visited_inner_classes();
    if let Some(source) = &original.source_file {
        cw.seed_utf8(source);
    }
    let enclosing = match &original.outer_class {
        Some(outer) => {
            let owner = remapper.map_type(&outer.owner)?;
            cw.seed_class(&owner);
            let method = match &outer.method {
                Some((name, desc)) => {
                    let desc = remapper.map_desc(desc)?;
                    cw.seed_name_and_type(name, &desc);
                    Some((name.clone(), desc))
                }
                None => None,
            };
            Some((owner, method))
        }
        None => None,
    };
    for (annotations, visible) in [
        (&original.visible_annotations, true),
        (&original.invisible_annotations, false),
    ] {
        for annotation in annotations {
            let mut annotation = annotation.clone();
            remapper.remap_annotation(&mut annotation)?;
            cw.add_copied_class_annotation(&annotation, visible);
        }
    }
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
        cw.seed_class(&spec.inner);
        if let Some(outer) = &spec.outer {
            cw.seed_class(outer);
        }
        if let Some(name) = &spec.name {
            cw.seed_utf8(name);
        }
        cw.add_inner_class(spec);
    }
    for field in &original.fields {
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
    for method in &original.methods {
        let mut method = method.clone();
        remap_declaration(&mut method, &mut remapper)?;
        if let Some(code) = &mut method.code {
            remapper.remap_method(code)?;
        }
        cw.add_copied_method(&method)
            .map_err(RegenerationError::Copy)?;
    }
    cw.set_source_file(original.source_file.clone());
    match enclosing {
        Some((owner, Some((name, desc)))) => cw.set_enclosing_method(&owner, &name, &desc),
        Some((owner, None)) => cw.set_enclosing_class(&owner),
        None => {}
    }
    Ok(cw.finish())
}
