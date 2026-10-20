//! Kotlin collection classifiers for types read from JVM signatures.
//!
//! A JVM signature spells every collection by its Java interface (`java/util/List`) and every
//! primitive type argument by its wrapper (`java/lang/Integer`), erasing the read-only/mutable split.
//! These restore the Kotlin classifiers the front end resolves against.

use crate::jvm::classpath::kotlin_name_to_ty;
use crate::jvm::jvm_class_map;
use crate::types::{Ty, TyList};

/// Overlay the `@Metadata`-declared collection classifiers onto a JVM-signature-derived type, level
/// by level. The signature erases read-only vs mutable (`List`/`MutableList` both spell
/// `java/util/List`) at EVERY nesting depth; the metadata type preserves it. At each level the
/// metadata classifier replaces the signature's name ONLY when the shared builtin-erasure table says
/// it is a Kotlin collection sibling mapping to the same JVM internal — guaranteeing the same
/// collection family and arity —
/// and the walk descends into type arguments only when the two classifiers agree (sibling or
/// identical) with matching arity, so a divergent classifier (stale metadata) never forms an
/// arity-mismatched or misaligned type. The base keeps its structure, primitives, and nullability;
/// only names are taken from metadata.
pub(super) fn overlay_metadata_collection_names(base: Ty, meta: Ty) -> Ty {
    // Nullability lives on the base (the resolution pipeline applies it separately); look through a
    // metadata `T?` to its classifier.
    let meta = meta.non_null();
    let (Ty::Obj(base_name, base_args), Ty::Obj(meta_name, meta_args)) = (base, meta) else {
        return base;
    };
    let sibling = jvm_class_map::is_kotlin_collection_type_name(meta_name)
        && jvm_class_map::type_names_map_to_same_jvm_internal(meta_name, base_name);
    if !sibling && base_name != meta_name {
        return base;
    }
    let name = if sibling { meta_name } else { base_name };
    if !base_args.is_empty() && base_args.len() == meta_args.len() {
        let merged: Vec<Ty> = base_args
            .iter()
            .zip(meta_args.iter())
            .map(|(&base_arg, &meta_arg)| overlay_metadata_collection_names(base_arg, meta_arg))
            .collect();
        Ty::obj_args_name(name, &merged)
    } else {
        // No arguments to align (a raw erased base) or an arity mismatch: the outer classifier is
        // still sound to take, the arguments stay the base's own.
        Ty::obj_args_name(name, base_args.as_slice())
    }
}

/// Recursively rewrite each JVM collection interface in `ty` (`java/util/List<T>` → its Kotlin
/// `kotlin/collections/List<T>`), leaving every other type and the type arguments' own structure intact.
pub(super) fn canonicalize_jvm_collections(ty: Ty) -> Ty {
    match ty {
        // A boxed primitive wrapper (`java/lang/Integer`) — always a type ARGUMENT here (a top-level
        // return is a collection with args, or handled by the suspend bare-class path). Its Kotlin type is
        // the PRIMITIVE (`kotlin/Int` → `Ty::Int`), not a boxed `Obj("kotlin/Int")`: a signature spells a
        // generic argument's primitive in its boxed JVM form (`List<Integer>`), but `List<Int>`'s element
        // is `Int`, so `for (x in xs) { s += x }` / `xs.sum()` resolve and the element unboxes rather than
        // comparing `Int` against a boxed reference.
        Ty::Obj(name, TyList::EMPTY) => match jvm_class_map::wrapper_to_kotlin_prim_name(name) {
            Some(prim) => kotlin_name_to_ty(prim),
            None => Ty::obj_name(
                jvm_class_map::jvm_collection_to_kotlin_type_name(name).unwrap_or(name),
            ),
        },
        Ty::Obj(name, args) => {
            // Canonicalize a JVM collection to its Kotlin form (`java/util/List` →
            // `kotlin/collections/List`), so a member/`for`/extension keyed on the Kotlin collection
            // resolves on the recovered type.
            let kname = jvm_class_map::jvm_collection_to_kotlin_type_name(name).unwrap_or(name);
            let cargs: Vec<Ty> = args
                .iter()
                .map(|a| canonicalize_jvm_collections(*a))
                .collect();
            Ty::obj_args_name(kname, &cargs)
        }
        Ty::Fun(sig) => {
            let params = sig
                .params
                .iter()
                .map(|param| canonicalize_jvm_collections(*param))
                .collect();
            let ret = canonicalize_jvm_collections(sig.ret);
            Ty::fun_with_shape(
                params,
                ret,
                sig.context_count,
                sig.has_receiver,
                sig.suspend,
            )
        }
        Ty::Nullable(inner) => Ty::nullable(canonicalize_jvm_collections(*inner)),
        other => other,
    }
}
