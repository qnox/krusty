//! `mapOf(...)` / `setOf(...)` and what a program does with the map or set it answers.
//!
//! A map is two growable lists side by side — its keys in insertion order and the values beside
//! them — and a set is the same object with no values, which is what Kotlin's own `LinkedHashSet`
//! is. The runtime answers their members (`src/native/runtime/krusty_rt.c`, the `maps and sets`
//! section), so `equals`, `hashCode` and `toString` agree with what the Kotlin declarations say.
//!
//! Which call reaches the runtime is decided by the RECEIVER's type, never by the owner declaring
//! the member — the position `lists.rs` already takes, and for the same reason: `get` is declared
//! on `Map`, which a user class may implement, and keying on the owner would send a user's object
//! into the runtime. A file that declares such an implementation is declined at every member asked
//! of a type it implements itself, before this is reached.

use super::*;

/// Whether a type is one of the runtime's maps, sets, or map entries. The provider-published
/// collection role is followed through concrete implementations' supertype graphs.
fn is_map(file: &FileLowering<'_>, ty: Ty) -> bool {
    file.mapped_collection(ty)
        .is_some_and(|collection| collection.kind == crate::types::CollectionKind::Map)
}

fn is_set(file: &FileLowering<'_>, ty: Ty) -> bool {
    file.mapped_collection(ty)
        .is_some_and(|collection| collection.kind == crate::types::CollectionKind::Set)
}

fn is_map_entry(file: &FileLowering<'_>, ty: Ty) -> bool {
    file.mapped_collection(ty)
        .is_some_and(|collection| collection.kind == crate::types::CollectionKind::MapEntry)
}

/// The runtime function answering one member of a map, with the types it is carried at.
///
/// `size` is an `Int` the generator must not box to ask about, which is why each signature is
/// spelled out rather than taken as all-references.
/// The runtime entry point for a map, set or entry member, chosen by the OWNER rather than by the
/// receiver.
///
/// Every other reader here keys on the receiver, because that is what says which object it is. This
/// one is for the dispatch that exists precisely because the receiver cannot be trusted — a file's
/// own class may be behind it — so the static type is all there is, and the owner is it.
pub(super) fn runtime_symbol(
    file: &FileLowering<'_>,
    receiver: crate::types::TypeName,
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    let collection = classifier_shapes::mapped_collection(file.classifiers, receiver)?;
    let declaration = signature
        .owner
        .semantic_classifier()
        .and_then(|owner| classifier_shapes::collection_declaration(file.classifiers, owner));
    if collection.kind == crate::types::CollectionKind::MapEntry {
        map_entry_symbol(declaration, signature)
    } else if collection.kind == crate::types::CollectionKind::Map {
        map_symbol(declaration?, signature)
    } else if collection.kind == crate::types::CollectionKind::Set {
        set_symbol(set_declaration(file, signature)?, signature)
    } else {
        None
    }
}

fn map_symbol(
    declaration: classifier_shapes::CollectionDeclaration,
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    use crate::types::CollectionKind::Map;
    let reads = declaration.reads(&[Map]);
    let mutates = declaration.mutates(&[Map]);
    Some(match (signature.name, signature.params, signature.ret) {
        // `Map.size` is a Kotlin property over a Java method, so the provider may present the
        // getter under either spelling; both name the same question.
        ("getSize" | "size", [], Ty::Int) if reads => ("kt_map_size", vec![any()], Ty::Int),
        ("isEmpty", [], Ty::Boolean) if reads => ("kt_map_is_empty", vec![any()], Ty::Boolean),
        ("get", [key], ret) if reads && key.is_reference() && ret.is_reference() => {
            ("kt_map_get", vec![any(), any()], any())
        }
        ("getOrDefault", [key, default], ret)
            if reads && key.is_reference() && default.is_reference() && ret.is_reference() =>
        {
            ("kt_map_get_or_default", vec![any(), any(), any()], any())
        }
        ("containsKey", [key], Ty::Boolean) if reads && key.is_reference() => {
            ("kt_map_contains_key", vec![any(), any()], Ty::Boolean)
        }
        ("containsValue", [value], Ty::Boolean) if reads && value.is_reference() => {
            ("kt_map_contains_value", vec![any(), any()], Ty::Boolean)
        }
        ("keys" | "getKeys", [], ret) if reads && ret.is_reference() => {
            ("kt_map_keys", vec![any()], any())
        }
        ("values" | "getValues", [], ret) if reads && ret.is_reference() => {
            ("kt_map_values", vec![any()], any())
        }
        ("entries" | "getEntries", [], ret) if reads && ret.is_reference() => {
            ("kt_map_entries", vec![any()], any())
        }
        // The mutating half. `put` answers the value that was there; `set` is `m[k] = v` and
        // answers `Unit`, so it is its own entry point rather than a result a caller has to
        // remember to drop.
        ("put", [key, value], ret)
            if mutates && key.is_reference() && value.is_reference() && ret.is_reference() =>
        {
            ("kt_map_put", vec![any(), any(), any()], any())
        }
        ("set", [key, value], Ty::Unit)
            if mutates && key.is_reference() && value.is_reference() =>
        {
            ("kt_map_set", vec![any(), any(), any()], Ty::Unit)
        }
        ("remove", [key], ret) if mutates && key.is_reference() && ret.is_reference() => {
            ("kt_map_remove", vec![any(), any()], any())
        }
        ("clear", [], Ty::Unit) if mutates => ("kt_map_clear", vec![any()], Ty::Unit),
        _ => return None,
    })
}

/// The top-level `MutableMap.set(key, value)` extension selected for indexed assignment.
///
/// Unlike `MutableMap.put`, this declaration belongs to the `kotlin.collections` package. Match
/// its complete generic signature so another package function named `set`, or an overload whose
/// receiver/arguments only happen to have the same runtime carriers, cannot enter the map runtime.
fn is_map_set_extension(signature: super::super::super::intrinsics::FunctionSignature<'_>) -> bool {
    if !signature.owner.package_matches("kotlin/collections")
        || signature.name != "set"
        || signature.ret != Ty::Unit
    {
        return false;
    }
    let Some(Ty::Obj(owner, arguments)) = signature.receiver.map(Ty::non_null) else {
        return false;
    };
    let [key, value] = arguments else {
        return false;
    };
    let [actual_key, actual_value] = signature.params else {
        return false;
    };
    super::super::super::intrinsics::classifier_matches(owner, "kotlin/collections/MutableMap")
        && key.canonical_semantic() == actual_key.canonical_semantic()
        && value.canonical_semantic() == actual_value.canonical_semantic()
}

/// The runtime function answering one member of a set.
///
/// A set's `size` and `isEmpty` are the map's: a set IS a map with no values, and both questions
/// are about its keys.
fn set_symbol(
    declaration: classifier_shapes::CollectionDeclaration,
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    use crate::types::CollectionKind::{Collection, Set};
    let reads = declaration.reads(&[Collection, Set]);
    let mutates = declaration.mutates(&[Collection, Set]);
    Some(match (signature.name, signature.params, signature.ret) {
        ("getSize" | "size", [], Ty::Int) if reads => ("kt_map_size", vec![any()], Ty::Int),
        ("isEmpty", [], Ty::Boolean) if reads => ("kt_map_is_empty", vec![any()], Ty::Boolean),
        ("contains", [element], Ty::Boolean) if reads && element.is_reference() => {
            ("kt_set_contains", vec![any(), any()], Ty::Boolean)
        }
        ("containsAll", [elements], Ty::Boolean) if reads && elements.is_reference() => (
            "kt_collection_contains_all",
            vec![any(), any()],
            Ty::Boolean,
        ),
        ("addAll", [elements], Ty::Boolean) if mutates && elements.is_reference() => (
            "kt_mutable_collection_add_all",
            vec![any(), any()],
            Ty::Boolean,
        ),
        ("add", [element], Ty::Boolean) if mutates && element.is_reference() => {
            ("kt_set_add", vec![any(), any()], Ty::Boolean)
        }
        ("remove", [element], Ty::Boolean) if mutates && element.is_reference() => {
            ("kt_set_remove", vec![any(), any()], Ty::Boolean)
        }
        ("clear", [], Ty::Unit) if mutates => ("kt_map_clear", vec![any()], Ty::Unit),
        _ => return None,
    })
}

/// The exact declaration a set member was selected from; see
/// [`classifier_shapes::collection_declaration`].
fn set_declaration(
    file: &FileLowering<'_>,
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<classifier_shapes::CollectionDeclaration> {
    classifier_shapes::collection_declaration(
        file.classifiers,
        signature.owner.semantic_classifier()?,
    )
}

/// The runtime function answering one member of a map entry.
///
/// `component1`/`component2` are what a destructuring reads, and are the same two questions under
/// the names the convention uses.
fn map_entry_symbol(
    declaration: Option<classifier_shapes::CollectionDeclaration>,
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    let extension_entry = if signature.owner.package_matches("kotlin/collections") {
        let Ty::Obj(owner, [key, value]) = signature.receiver?.non_null() else {
            return None;
        };
        if !super::super::super::intrinsics::classifier_matches(
            owner,
            "kotlin/collections/Map$Entry",
        ) {
            return None;
        }
        Some((*key, *value))
    } else {
        None
    };
    let reads = declaration
        .is_some_and(|declaration| declaration.reads(&[crate::types::CollectionKind::MapEntry]));
    if !reads && extension_entry.is_none() {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("component1", [], ret)
            if extension_entry
                .is_some_and(|(key, _)| key.canonical_semantic() == ret.canonical_semantic()) =>
        {
            ("kt_map_entry_key", vec![any()], any())
        }
        ("component2", [], ret)
            if extension_entry.is_some_and(|(_, value)| {
                value.canonical_semantic() == ret.canonical_semantic()
            }) =>
        {
            ("kt_map_entry_value", vec![any()], any())
        }
        ("key" | "getKey", [], ret) if reads && ret.is_reference() => {
            ("kt_map_entry_key", vec![any()], any())
        }
        ("value" | "getValue", [], ret) if reads && ret.is_reference() => {
            ("kt_map_entry_value", vec![any()], any())
        }
        _ => return None,
    })
}

/// The type one of those getters answers with, for the physical-type question.
#[derive(Clone, Copy)]
pub(super) struct MapProperty {
    symbol: &'static str,
    answer: Ty,
    name: &'static str,
}

impl MapProperty {
    pub(super) fn answer(self) -> Ty {
        self.answer
    }

    pub(super) fn name(self) -> &'static str {
        self.name
    }

    fn from_runtime(symbol: &'static str, answer: Ty) -> Option<Self> {
        let name = match symbol {
            "kt_map_size" => "size",
            "kt_map_keys" => "keys",
            "kt_map_values" => "values",
            "kt_map_entries" => "entries",
            "kt_map_entry_key" => "key",
            "kt_map_entry_value" => "value",
            _ => return None,
        };
        Some(Self {
            symbol,
            answer,
            name,
        })
    }
}

impl BodyLowering<'_, '_, '_> {
    /// `mapOf(…)` / `setOf(…)` and their relatives, or `None` when the declaration is something
    /// else.
    ///
    /// A vararg call arrives with its operands already packed into an `Array<T>`, and the runtime
    /// copies out of it: the array belongs to the caller, and a map can be written through.
    pub(super) fn map_construction(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        packs_a_vararg: bool,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_collections_facade(signature.owner)
            || !signature.ret.is_reference()
        {
            return None;
        }
        match (signature.name, signature.params, args) {
            // The empty forms. `mapOf()` with nothing in it is Kotlin's `emptyMap()`, and the
            // three named maps are one object here.
            ("emptyMap" | "mapOf", [], []) => {
                Some(self.runtime_call("kt_map_new", &[], any(), &[]))
            }
            ("mapOf" | "mutableMapOf" | "hashMapOf", [_], []) if packs_a_vararg => {
                Some(self.runtime_call("kt_map_new", &[], any(), &[]))
            }
            ("mutableMapOf" | "hashMapOf", [], []) => {
                Some(self.runtime_call("kt_map_new", &[], any(), &[]))
            }
            ("emptySet" | "setOf", [], []) => {
                Some(self.runtime_call("kt_set_new", &[], any(), &[]))
            }
            ("setOf" | "mutableSetOf" | "hashSetOf", [_], []) if packs_a_vararg => {
                Some(self.runtime_call("kt_set_new", &[], any(), &[]))
            }
            ("mutableSetOf" | "hashSetOf", [], []) => {
                Some(self.runtime_call("kt_set_new", &[], any(), &[]))
            }
            ("mapOf" | "mutableMapOf" | "hashMapOf", [_], [argument]) if packs_a_vararg => {
                Some(self.built_from("kt_map_of", *argument))
            }
            ("setOf" | "mutableSetOf" | "hashSetOf", [_], [argument]) if packs_a_vararg => {
                Some(self.built_from("kt_set_of", *argument))
            }
            // The single-operand forms, which Kotlin declares beside the vararg ones. Only the
            // selected declaration's PHYSICAL parameter can say which this is, for the reason
            // `listOf` spells out: a vararg parameter is an array however its element type was
            // substituted, and `T` may itself be an array.
            ("mapOf", [pair], [value]) if pair.is_reference() => {
                Some(self.built_from("kt_map_of_pair", *value))
            }
            ("setOf", [element], [value]) if element.is_reference() => {
                Some(self.set_single(*value))
            }
            _ => None,
        }
    }

    /// One of those, over the array a vararg call packed.
    fn built_from(&mut self, symbol: &str, packed: u32) -> Result<Option<Value>, Unsupported> {
        let elements = self.reference(packed)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(symbol, &[any()], any(), &[elements])
    }

    /// `setOf(x)`: the one-element form, built by adding to an empty set rather than by packing an
    /// array first — there is nothing for an array to carry that the element does not.
    fn set_single(&mut self, element: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(element)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(set) = self.runtime_call("kt_set_new", &[], any(), &[])? else {
            return Ok(None);
        };
        self.runtime_call("kt_set_add", &[any(), any()], Ty::Boolean, &[set, value])?;
        Ok(Some(set))
    }

    /// A member of a runtime map, set or map entry, or `None` when the receiver is none of them.
    pub(super) fn map_member(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let ty = self.type_of(receiver)?;
        // A file that declares its own map, set or entry puts an object of ITS own behind that
        // type, and the runtime would read a header that is not there. No static type tells the
        // two apart within a shape, so a receiver of a shape the file implements declines — the
        // same position `lists.rs` takes, and by the same test.
        if self.file.implements_collection_of(ty) {
            return None;
        }
        let declaration = signature.owner.semantic_classifier().and_then(|owner| {
            classifier_shapes::collection_declaration(self.file.classifiers, owner)
        });
        let (symbol, carried, answer) = if is_map(self.file, ty) {
            if is_map_set_extension(signature) {
                ("kt_map_set", vec![any(), any(), any()], Ty::Unit)
            } else {
                map_symbol(declaration?, signature)?
            }
        } else if is_set(self.file, ty) {
            set_symbol(set_declaration(self.file, signature)?, signature)?
        } else if is_map_entry(self.file, ty) {
            map_entry_symbol(declaration, signature)?
        } else {
            return None;
        };
        Some(self.map_call(symbol, &carried, answer, receiver, args, ret))
    }

    /// A checked dependency-property read over the runtime's map/set representation. The selected
    /// property identity is validated by [`Self::map_getter`] before this carrier-only call.
    pub(super) fn map_property_member(
        &mut self,
        property: MapProperty,
        receiver: u32,
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        Some(self.map_call(
            property.symbol,
            &[any()],
            property.answer,
            receiver,
            &[],
            ret,
        ))
    }

    /// A checked read of a map's or set's property the runtime answers — `m.size`, `m.keys` — by
    /// the RECEIVER, as every other one here is.
    pub(super) fn map_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<MapProperty> {
        let ty = self.type_of(receiver)?;
        if self.file.implements_collection_of(ty)
            || !(is_map(self.file, ty) || is_set(self.file, ty) || is_map_entry(self.file, ty))
        {
            return None;
        }
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        let Some(crate::types::SemanticCallableOwner::Classifier(owner)) = getter.declaration_owner
        else {
            return None;
        };
        let declaration = classifier_shapes::collection_declaration(self.file.classifiers, owner)?;
        let generic = getter.generic_sig.as_deref();
        let params = generic
            .map(|signature| signature.params.as_slice())
            .or(getter.declared_params.as_deref())
            .unwrap_or(&getter.params);
        let result = generic
            .map(|signature| signature.ret)
            .or(getter.declared_ret)
            .unwrap_or(getter.ret);
        let name = getter.reflection_name.as_deref().unwrap_or(&getter.name);
        let signature = super::super::super::intrinsics::FunctionSignature::new(
            super::super::super::intrinsics::DeclarationOwner::callable(
                getter.physical_owner,
                getter.declaration_owner,
            ),
            name,
            params,
            result,
        );
        let kind = self.file.mapped_collection(ty)?.kind;
        let (symbol, carried, answer) = match kind {
            crate::types::CollectionKind::Map => map_symbol(declaration, signature)?,
            crate::types::CollectionKind::Set => set_symbol(declaration, signature)?,
            crate::types::CollectionKind::MapEntry => {
                map_entry_symbol(Some(declaration), signature)?
            }
            _ => return None,
        };
        if carried.len() != 1 || self.file.carrier(property.result) != self.file.carrier(answer) {
            return None;
        }
        MapProperty::from_runtime(symbol, answer)
    }

    fn map_call(
        &mut self,
        symbol: &str,
        carried: &[Ty],
        answer: Ty,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let mut operands = vec![self.reference(receiver)?];
        for (argument, ty) in args.iter().zip(&carried[1..]) {
            let Some(value) = self.coerce(*argument, *ty)? else {
                return Err(format!("a `Unit` operand of `{symbol}`"));
            };
            operands.push(value);
        }
        if self.terminated {
            return Ok(None);
        }
        let Some(value) = self.runtime_call(symbol, carried, answer, &operands)? else {
            return Ok(None);
        };
        self.convert(value, Some(answer), ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature<'a>(
        owner: &str,
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
    ) -> crate::native::intrinsics::FunctionSignature<'a> {
        crate::native::intrinsics::FunctionSignature::new(
            crate::native::intrinsics::DeclarationOwner::callable(
                crate::types::type_name(owner),
                None,
            ),
            name,
            params,
            ret,
        )
    }

    #[test]
    fn map_runtime_members_require_complete_signatures() {
        use classifier_shapes::CollectionDeclaration;
        let key = Ty::ty_param("K", any());
        let value = Ty::ty_param("V", any());
        let map = CollectionDeclaration::Interface(crate::types::MappedCollection {
            kind: crate::types::CollectionKind::Map,
            mutable: false,
        });
        assert!(map_symbol(
            map,
            signature("kotlin/collections/Map", "get", &[key], value),
        )
        .is_some());
        assert!(map_symbol(
            map,
            signature("kotlin/collections/Map", "get", &[Ty::Int], value),
        )
        .is_none());
        assert!(map_symbol(
            map,
            signature("kotlin/collections/Map", "get", &[key], Ty::Boolean),
        )
        .is_none());
        assert!(map_symbol(
            CollectionDeclaration::Interface(crate::types::MappedCollection {
                kind: crate::types::CollectionKind::Set,
                mutable: false,
            }),
            signature("kotlin/collections/Map", "get", &[key], value),
        )
        .is_none());
    }

    #[test]
    fn map_members_match_their_exact_declaration_face() {
        use classifier_shapes::{CollectionDeclaration, StandardCollectionImplementation};
        let face = |mutable| {
            CollectionDeclaration::Interface(crate::types::MappedCollection {
                kind: crate::types::CollectionKind::Map,
                mutable,
            })
        };
        let size = signature("kotlin/collections/Map", "size", &[], Ty::Int);
        let clear = signature("kotlin/collections/MutableMap", "clear", &[], Ty::Unit);
        let hash_map = CollectionDeclaration::Implementation(StandardCollectionImplementation::Map);
        assert!(map_symbol(face(false), size).is_some());
        assert!(map_symbol(face(true), size).is_none());
        assert!(map_symbol(face(true), clear).is_some());
        assert!(map_symbol(face(false), clear).is_none());
        assert!(map_symbol(hash_map, size).is_some());
        assert!(map_symbol(hash_map, clear).is_some());
        assert!(map_symbol(
            CollectionDeclaration::Implementation(StandardCollectionImplementation::Set),
            size,
        )
        .is_none());
    }

    #[test]
    fn set_members_match_their_declaration_face() {
        use classifier_shapes::{CollectionDeclaration, StandardCollectionImplementation};
        let face = |mutable| {
            CollectionDeclaration::Interface(crate::types::MappedCollection {
                kind: crate::types::CollectionKind::Set,
                mutable,
            })
        };
        let size = signature("kotlin/collections/Set", "size", &[], Ty::Int);
        let clear = signature("kotlin/collections/MutableSet", "clear", &[], Ty::Unit);
        let hash_set = CollectionDeclaration::Implementation(StandardCollectionImplementation::Set);
        assert!(set_symbol(face(false), size).is_some());
        assert!(set_symbol(face(true), size).is_none());
        assert!(set_symbol(face(true), clear).is_some());
        assert!(set_symbol(face(false), clear).is_none());
        assert!(set_symbol(hash_set, clear).is_some());
        assert!(set_symbol(
            CollectionDeclaration::Implementation(StandardCollectionImplementation::List),
            size
        )
        .is_none());
    }

    #[test]
    fn map_set_extension_requires_its_complete_signature() {
        let key = Ty::ty_param("K", any());
        let value = Ty::ty_param("V", any());
        let mutable_map = Ty::obj_args("kotlin/collections/MutableMap", &[key, value]);
        let package = crate::native::intrinsics::DeclarationOwner::callable(
            crate::types::wk::kotlin_collections_package(),
            Some(crate::types::SemanticCallableOwner::Package(
                crate::types::wk::kotlin_collections_package(),
            )),
        );
        let exact_params = [key, value];
        let exact = crate::native::intrinsics::FunctionSignature::new(
            package,
            "set",
            &exact_params,
            Ty::Unit,
        )
        .with_receiver(Some(mutable_map));
        assert!(is_map_set_extension(exact));

        assert!(!is_map_set_extension(
            crate::native::intrinsics::FunctionSignature::new(
                package,
                "set",
                &[key, Ty::Int],
                Ty::Unit,
            )
            .with_receiver(Some(mutable_map)),
        ));
        assert!(!is_map_set_extension(
            crate::native::intrinsics::FunctionSignature::new(
                package,
                "set",
                &[key, value],
                Ty::Boolean,
            )
            .with_receiver(Some(mutable_map)),
        ));
    }
}
