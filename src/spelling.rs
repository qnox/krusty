//! How a DECLARED type was SPELLED in source, carried alongside the expanded [`Ty`] it resolved to.
//!
//! Kotlin's `@Metadata` records both forms of a declared type: the expanded classifier
//! (`Type.class_name`) and, when the source named a `typealias`, the spelling
//! (`Type.abbreviated_type` = field 13, whose `Type.type_alias_name` = field **12** names the
//! alias). `fun make(c: Cargo): Cargo` with `typealias Cargo = Payload` writes
//! `Type{class_name=Payload, abbreviated_type=Type{type_alias_name=Cargo}}`.
//!
//! This lives BESIDE `Ty` rather than inside it. [`Ty`](crate::types::Ty) is `Copy + PartialEq + Eq
//! + Hash` and interned, and the whole compiler compares types structurally; an alias slot would
//! make `Obj(Payload, alias = Cargo) != Obj(Payload, alias = None)` and silently split every type
//! comparison, interner bucket, and hash lookup on a distinction that is pure surface syntax.
//! kotlinc keeps abbreviation off type equality for the same reason. Nothing outside metadata
//! emission may read this: the expanded `Ty` stays the single semantic truth.
//!
//! Field numbers here were read off kotlinc 2.4.10 output, not recalled — see
//! `docs/METADATA_NOTES.md`.

use crate::types::{Ty, TypeName};

/// The source spelling of one declared type, mirroring the expanded `Ty` tree node for node.
///
/// The default (`alias: None`, both argument lists empty) means "no alias was spelled at or below
/// this node", which is the overwhelmingly common case and allocates nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spelled {
    /// This occurrence was written as a definitely-non-null intersection (`T & Any`). The semantic
    /// [`Ty`] already carries that fact in its occurrence bound; this sidecar preserves the exact
    /// Kotlin-metadata flag without making it part of structural type identity.
    pub definitely_non_null: bool,
    /// The `typealias` named AT this node, fully qualified.
    ///
    /// kotlinc records only the OUTERMOST alias of a chain: with `typealias Cargo = Payload` and
    /// `typealias Chain = Cargo`, a declared `Chain` writes `type_alias_name = Chain`, never a
    /// nested `Cargo`. So this is one level and never itself an alias-to-an-alias reference.
    pub alias: Option<TypeName>,
    /// The alias's AS-SPELLED type arguments, which differ in ARITY from the expanded type's:
    /// `Boxed<Int>` with `typealias Boxed<T> = PBox<T, T>` has ONE entry here and TWO expanded
    /// arguments. These become the abbreviated `Type`'s own `argument` list, so each carries the
    /// `Ty` it resolved to as well as its own spelling. Empty when `alias` is `None`.
    pub alias_args: Vec<(Ty, Spelled)>,
    /// Spellings of the EXPANDED type's arguments, positionally parallel to
    /// [`Ty::type_args`](crate::types::Ty::type_args).
    ///
    /// Populated even when `alias` is `Some`: an alias whose right-hand side itself spells an alias
    /// propagates that spelling into the expansion. `typealias CargoBox = PBox<Cargo, Cargo>` used
    /// as `CargoBox` writes abbreviations on BOTH expanded arguments as well as on the node itself.
    /// A short or empty vec leaves the remaining arguments unabbreviated.
    pub args: Vec<Spelled>,
}

impl Spelled {
    /// The "nothing was spelled as an alias" spelling — usable as a `&'static` argument at the many
    /// encode sites whose types cannot carry an alias (synthesized members, builtin classifiers).
    pub const NONE: &'static Spelled = &Spelled {
        definitely_non_null: false,
        alias: None,
        alias_args: Vec::new(),
        args: Vec::new(),
    };

    /// Whether this node and everything below it is free of alias spellings — the fast path that
    /// lets the encoder skip the parallel walk entirely.
    pub fn is_none(&self) -> bool {
        !self.definitely_non_null && self.alias.is_none() && self.args.iter().all(Spelled::is_none)
    }

    /// This node's spelling for the type argument at `index`, or the empty spelling when the
    /// argument list is short (the common "only some arguments spelled an alias" shape).
    pub fn arg(&self, index: usize) -> &Spelled {
        self.args.get(index).unwrap_or(Spelled::NONE)
    }

    /// This spelling lifted to describe an `Array<Self>`: the array itself spells no alias, its
    /// sole ELEMENT does.
    ///
    /// A `vararg xs: Cargo` parameter is SPELLED as the element but RECORDED as `Array<Cargo>`, so
    /// applying the element's spelling to the array directly would claim the array was the alias.
    pub fn as_array_element(&self) -> Spelled {
        Spelled {
            definitely_non_null: false,
            alias: None,
            alias_args: Vec::new(),
            args: vec![self.clone()],
        }
    }

    /// A spelling that names `alias` at this node with no arguments — the plain
    /// `typealias Cargo = Payload` case.
    pub fn of_alias(alias: TypeName) -> Spelled {
        Spelled {
            alias: Some(alias),
            ..Spelled::default()
        }
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        self.alias_args.len() * std::mem::size_of::<(Ty, Spelled)>()
            + self.args.len() * std::mem::size_of::<Spelled>()
            + self
                .alias_args
                .iter()
                .map(|(_, spelling)| spelling.storage_payload_bytes())
                .sum::<usize>()
            + self
                .args
                .iter()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
    }
}

/// Source spellings of ONE declaration's declared types, addressed the way the metadata builders
/// address them.
///
/// Every field defaults to "nothing spelled an alias", and a declaration whose whole record would
/// be empty is not recorded at all — so the overwhelming majority of declarations, in the
/// overwhelming majority of modules, cost nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclaredSpellings {
    /// The declared return type (a property's type, for a property).
    pub ret: Spelled,
    /// Parallel to the declaration's LOGICAL value parameters — an extension receiver is recorded
    /// in [`Self::receiver`], not as a leading parameter, matching how `@Metadata` separates them.
    pub params: Vec<Spelled>,
    /// The extension receiver's spelling (`val Cargo.ext: Cargo` abbreviates both).
    pub receiver: Spelled,
    /// Per declared type parameter, that parameter's declared upper bounds (`<T : Cargo>`).
    pub type_param_bounds: Vec<Vec<Spelled>>,
    /// Class headers only: the declared SUPERCLASS (`class Sub : Super()`), empty when the class
    /// declares none. Kept apart from [`Self::supertypes`] because the emitted supertype list has a
    /// slot for it only when a superclass was declared — and a superclass that declared no alias
    /// spells nothing here, so this field alone cannot say whether that slot exists. Only the
    /// emitter knows.
    pub superclass: Spelled,
    /// Class headers only: the declared INTERFACES, in declaration order.
    pub supertypes: Vec<Spelled>,
}

impl DeclaredSpellings {
    /// The empty record, for the many builder paths whose declaration spelled no alias.
    pub const NONE: &'static DeclaredSpellings = &DeclaredSpellings {
        ret: Spelled {
            definitely_non_null: false,
            alias: None,
            alias_args: Vec::new(),
            args: Vec::new(),
        },
        params: Vec::new(),
        receiver: Spelled {
            definitely_non_null: false,
            alias: None,
            alias_args: Vec::new(),
            args: Vec::new(),
        },
        superclass: Spelled {
            definitely_non_null: false,
            alias: None,
            alias_args: Vec::new(),
            args: Vec::new(),
        },
        type_param_bounds: Vec::new(),
        supertypes: Vec::new(),
    };

    /// Whether this declaration spelled no `typealias` anywhere — the signal not to record it.
    pub fn is_none(&self) -> bool {
        self.ret.is_none()
            && self.receiver.is_none()
            && self.superclass.is_none()
            && self.params.iter().all(Spelled::is_none)
            && self.supertypes.iter().all(Spelled::is_none)
            && self
                .type_param_bounds
                .iter()
                .all(|bounds| bounds.iter().all(Spelled::is_none))
    }

    pub fn param(&self, index: usize) -> &Spelled {
        self.params.get(index).unwrap_or(Spelled::NONE)
    }

    pub fn supertype(&self, index: usize) -> &Spelled {
        self.supertypes.get(index).unwrap_or(Spelled::NONE)
    }

    /// The spellings for an emitted supertype list, aligned to it.
    ///
    /// `has_declared_superclass` says whether that list leads with a superclass, which it does
    /// exactly when the class declared one — a superclass position is never materialized for an
    /// undeclared `kotlin/Any`. It cannot be inferred from [`Self::superclass`]: a declared
    /// superclass that named no `typealias` spells nothing, yet still occupies the slot. Getting
    /// this wrong shifts every abbreviation onto the neighbouring supertype.
    pub fn supertype_spellings(&self, has_declared_superclass: bool) -> Vec<Spelled> {
        let superclass = has_declared_superclass.then(|| self.superclass.clone());
        superclass
            .into_iter()
            .chain(self.supertypes.iter().cloned())
            .collect()
    }

    pub fn bound(&self, parameter: usize, index: usize) -> &Spelled {
        self.type_param_bounds
            .get(parameter)
            .and_then(|bounds| bounds.get(index))
            .unwrap_or(Spelled::NONE)
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        self.ret.storage_payload_bytes()
            + self.receiver.storage_payload_bytes()
            + self.superclass.storage_payload_bytes()
            + self
                .params
                .iter()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
            + self
                .type_param_bounds
                .iter()
                .flatten()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
            + self
                .supertypes
                .iter()
                .map(Spelled::storage_payload_bytes)
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_spelling_is_none_and_yields_none_for_every_argument() {
        let spelled = Spelled::default();
        assert!(spelled.is_none());
        assert!(spelled.arg(0).is_none());
        assert!(spelled.arg(7).is_none());
    }

    #[test]
    fn an_undeclared_superclass_takes_no_slot_generic_or_not() {
        // `class Holder<T> : Marker` and `class Holder : Marker` emit the SAME supertype list — the
        // interface alone. A reserved slot for the undeclared `kotlin/Any` would land the
        // interface's abbreviation one position early.
        let marker = Spelled::of_alias(crate::types::type_name("app/Marker"));
        let header = DeclaredSpellings {
            superclass: Spelled::default(),
            supertypes: vec![marker.clone()],
            ..DeclaredSpellings::default()
        };
        assert_eq!(header.supertype_spellings(false), vec![marker]);
    }

    #[test]
    fn a_declared_superclass_leads_even_when_it_spells_no_alias() {
        // `class Sub : Base(), Marker` with a plain `Base`: the superclass spells nothing, but it
        // still OCCUPIES the leading position, so `Marker`'s abbreviation must not slide onto it.
        let marker = Spelled::of_alias(crate::types::type_name("app/Marker"));
        let header = DeclaredSpellings {
            superclass: Spelled::default(),
            supertypes: vec![marker.clone()],
            ..DeclaredSpellings::default()
        };
        let aligned = header.supertype_spellings(true);
        assert_eq!(aligned.len(), 2, "the declared superclass holds a slot");
        assert!(aligned[0].is_none(), "a plain superclass spells nothing");
        assert_eq!(aligned[1], marker, "the interface keeps its own spelling");
    }

    #[test]
    fn a_declared_superclass_that_spells_an_alias_leads() {
        let base = Spelled::of_alias(crate::types::type_name("app/Super"));
        let header = DeclaredSpellings {
            superclass: base.clone(),
            supertypes: Vec::new(),
            ..DeclaredSpellings::default()
        };
        assert_eq!(header.supertype_spellings(true), vec![base]);
    }

    #[test]
    fn a_vararg_element_spelling_lifts_under_the_array() {
        // `vararg xs: Cargo` is spelled as the ELEMENT but recorded as `Array<Cargo>`, so the
        // spelling has to move under the array — applying it directly would claim the ARRAY was
        // written as the alias.
        let element = Spelled::of_alias(crate::types::type_name("app/Cargo"));
        let array = element.as_array_element();
        assert_eq!(array.alias, None, "the array itself spells no alias");
        assert_eq!(array.arg(0), &element, "the element keeps the spelling");
        assert!(!array.is_none(), "an alias below the root still counts");
    }

    #[test]
    fn an_alias_below_the_root_makes_the_tree_not_none() {
        // `List<Cargo>`: the root spells `List` directly, the ARGUMENT spells the alias. kotlinc
        // puts `abbreviated_type` on the argument's `Type`, so the root must not report "none".
        let spelled = Spelled {
            args: vec![Spelled::of_alias(crate::types::type_name("dep/Cargo"))],
            ..Spelled::default()
        };
        assert!(!spelled.is_none());
        assert_eq!(
            spelled.arg(0).alias,
            Some(crate::types::type_name("dep/Cargo"))
        );
        assert!(spelled.arg(1).is_none());
    }
}
