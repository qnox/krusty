//! The `toArray` members of a collection class (kotlinc's `ToArrayLowering`).
//!
//! Kotlin's `Collection` declares no `toArray`, but `java.util.Collection` does, so kotlinc gives
//! every class that implements a collection face, read-only or mutable and abstract or not, a
//! public `<T> T[] toArray(T[])` and a public `Object[] toArray()` that delegate to
//! `kotlin.jvm.internal.CollectionToArray`. A class whose superclass chain already has a collection
//! class inherits that superclass's members and gets none. A member that matches one of the two
//! shapes, declared by the class or inherited from any non-collection supertype, suppresses that one.
//! The shapes are the declaration's, never the name alone: `toArray(): Array<…>` with no type
//! parameters, and `<T> toArray(Array<T>): Array<T>`.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{IrClass, IrFile};
use crate::jvm::classpath::Classpath;
use crate::jvm::jvm_class_map::{
    is_mapped_collection_face, jvm_collection_to_kotlin_type_name, mapped_collection,
    to_jvm_type_name,
};
use crate::types::{wk, CollectionKind, Ty, TyKind, TypeName, Visibility};

const NAME: &str = "toArray";
const ACC_STATIC: u16 = 0x0008;
const ACC_PRIVATE: u16 = 0x0002;

/// Which of the two `toArray` members a class realizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ToArrayMembers {
    /// `public <T> T[] toArray(T[])`.
    pub(super) generic: bool,
    /// `public Object[] toArray()`.
    pub(super) non_generic: bool,
}

#[derive(Default)]
pub(super) struct CollectionToArray {
    by_class: HashMap<TypeName, ToArrayMembers>,
}

impl CollectionToArray {
    pub(super) fn for_class(&self, class: TypeName) -> ToArrayMembers {
        self.by_class.get(&class).copied().unwrap_or_default()
    }
}

pub(super) fn select(ir: &IrFile, classpath: &Classpath) -> CollectionToArray {
    let mut result = CollectionToArray::default();
    for (class_index, class) in ir.classes.iter().enumerate() {
        if !crate::jvm::override_results::realizes_overrides(ir, class_index)
            || class.is_interface
            || class.is_annotation
        {
            continue;
        }
        let Some(hierarchy) = ir.classifier_hierarchies.get(&class.fq_name_id()) else {
            continue;
        };
        let supertypes: Vec<TypeName> = hierarchy
            .iter()
            .filter(|entry| entry.depth > 0)
            .map(|entry| entry.classifier)
            .collect();
        if !supertypes.iter().copied().any(is_collection) {
            continue;
        }
        if superclass_is_collection(ir, classpath, class) {
            continue;
        }
        let shapes = existing_shapes(ir, classpath, class, &supertypes);
        let members = ToArrayMembers {
            generic: !shapes.generic,
            non_generic: !shapes.non_generic,
        };
        if members != ToArrayMembers::default() {
            result.by_class.insert(class.fq_name_id(), members);
        }
    }
    result
}

/// `kotlin.collections.Collection` in any of its faces, `java.util.Collection` included.
fn is_collection(classifier: TypeName) -> bool {
    jvm_collection_to_kotlin_type_name(to_jvm_type_name(classifier))
        .and_then(mapped_collection)
        .is_some_and(|mapped| mapped.kind == CollectionKind::Collection)
}

/// kotlinc's indirect collection subclass: some class in the superclass chain is a collection.
fn superclass_is_collection(ir: &IrFile, classpath: &Classpath, class: &IrClass) -> bool {
    let mut current = class.superclass;
    let mut seen = HashSet::default();
    while current != wk::java_object() && current != wk::any() && seen.insert(current) {
        if let Some(source) = ir.classes.iter().find(|c| c.fq_name_id() == current) {
            let collection = ir
                .classifier_hierarchies
                .get(&current)
                .is_some_and(|hierarchy| {
                    hierarchy
                        .iter()
                        .any(|entry| is_collection(entry.classifier))
                });
            if collection {
                return true;
            }
            current = source.superclass;
            continue;
        }
        return classpath_is_collection(classpath, current);
    }
    false
}

/// Whether a dependency class implements a collection, through its class-file supertypes.
fn classpath_is_collection(classpath: &Classpath, start: TypeName) -> bool {
    let mut pending = vec![start];
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if is_collection(current) {
            return true;
        }
        if let Some(info) = classpath.find_name(current) {
            pending.extend(info.super_class);
            pending.extend(info.interfaces.iter());
        }
    }
    false
}

#[derive(Default)]
struct Shapes {
    generic: bool,
    non_generic: bool,
}

impl Shapes {
    fn record(&mut self, formals: &[&str], extension: bool, parameters: &[Ty], result: Ty) {
        if extension {
            return;
        }
        match (formals, parameters) {
            ([], []) => self.non_generic |= result.non_null().is_reference_array(),
            ([formal], [parameter]) => {
                self.generic |= array_of(*parameter, formal) && array_of(result, formal);
            }
            _ => {}
        }
    }
}

/// `Array<T>` or `Array<T>?` of the function's own type parameter `T`.
fn array_of(ty: Ty, formal: &str) -> bool {
    let ty = ty.non_null();
    if !ty.is_reference_array() {
        return false;
    }
    let Some(element) = ty.array_elem() else {
        return false;
    };
    let element = match element.kind() {
        TyKind::OutProjection(inner) | TyKind::InProjection(inner) => inner,
        _ => element,
    };
    matches!(element.non_null().kind(), TyKind::TyParam(name, _) if name == formal)
}

/// The `toArray` shapes the class declares or inherits from a supertype that is not itself one of
/// the mapped collection interfaces (whose JDK `toArray` Kotlin does not see).
fn existing_shapes(
    ir: &IrFile,
    classpath: &Classpath,
    class: &IrClass,
    supertypes: &[TypeName],
) -> Shapes {
    let mut shapes = Shapes::default();
    source_shapes(ir, class, true, &mut shapes);
    for &supertype in supertypes {
        if is_mapped_collection_face(supertype) {
            continue;
        }
        if let Some(source) = ir.classes.iter().find(|c| c.fq_name_id() == supertype) {
            source_shapes(ir, source, false, &mut shapes);
            continue;
        }
        let Some(info) = classpath.find_name(supertype) else {
            continue;
        };
        if info.meta.class_kind.is_some() {
            for function in info.meta.class_functions.iter() {
                if function.kotlin_name != NAME || function.visibility == Visibility::Private {
                    continue;
                }
                let Some(signature) = function.generic_sig.as_ref() else {
                    continue;
                };
                let formals: Vec<&str> = signature.formals.iter().map(String::as_str).collect();
                shapes.record(
                    &formals,
                    function.is_extension(),
                    &signature.params,
                    signature.ret,
                );
            }
        } else {
            for method in &info.methods {
                if method.name != NAME || method.access & (ACC_STATIC | ACC_PRIVATE) != 0 {
                    continue;
                }
                match method.signature.as_deref() {
                    None => shapes.non_generic |= method.descriptor.starts_with("()["),
                    Some(signature) => shapes.generic |= java_generic_shape(signature),
                }
            }
        }
    }
    shapes
}

fn source_shapes(ir: &IrFile, class: &IrClass, own: bool, shapes: &mut Shapes) {
    for &function_id in &class.methods {
        let function = &ir.functions[function_id as usize];
        if function.is_static
            || function.name != NAME
            || (!own && ir.method_visibility(function_id) == Visibility::Private)
        {
            continue;
        }
        let extension = ir.extension_receiver_fns.contains(&function_id);
        match ir.signatures.get(&function_id) {
            Some(signature) => {
                let formals: Vec<&str> = signature
                    .type_params
                    .iter()
                    .map(|parameter| parameter.semantic_name.as_str())
                    .collect();
                let result = signature.ret.unwrap_or(function.ret);
                shapes.record(&formals, extension, &signature.params, result);
            }
            None => shapes.record(&[], extension, &function.params, function.ret),
        }
    }
}

/// A Java method `<T:…>([TT;)[TT;`.
fn java_generic_shape(signature: &str) -> bool {
    let Some(rest) = signature.strip_prefix('<') else {
        return false;
    };
    let Some((formal, rest)) = rest.split_once(':') else {
        return false;
    };
    let Some((_, method)) = rest.split_once('>') else {
        return false;
    };
    method == format!("([T{formal};)[T{formal};")
}
