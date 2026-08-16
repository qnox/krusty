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
        alias: None,
        alias_args: Vec::new(),
        args: Vec::new(),
    };

    /// Whether this node and everything below it is free of alias spellings — the fast path that
    /// lets the encoder skip the parallel walk entirely.
    pub fn is_none(&self) -> bool {
        self.alias.is_none() && self.args.iter().all(Spelled::is_none)
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
    /// declares none. Kept apart from [`Self::supertypes`] because the emitted supertype list does
    /// not always have a slot for it: a generic class always leads with the superclass position
    /// (holding `kotlin/Any` when undeclared) while a non-generic one omits that position
    /// entirely, so only the emitter can align the two lists.
    pub superclass: Spelled,
    /// Class headers only: the declared INTERFACES, in declaration order.
    pub supertypes: Vec<Spelled>,
}

impl DeclaredSpellings {
    /// The empty record, for the many builder paths whose declaration spelled no alias.
    pub const NONE: &'static DeclaredSpellings = &DeclaredSpellings {
        ret: Spelled {
            alias: None,
            alias_args: Vec::new(),
            args: Vec::new(),
        },
        params: Vec::new(),
        receiver: Spelled {
            alias: None,
            alias_args: Vec::new(),
            args: Vec::new(),
        },
        superclass: Spelled {
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
    /// `leads_with_superclass_slot` says whether that list reserves its first position for the
    /// declared superclass even when none was declared — which a generic class does (the position
    /// holds `kotlin/Any`) and a non-generic one does not. Getting this wrong shifts every
    /// abbreviation onto the neighbouring supertype.
    pub fn supertype_spellings(&self, leads_with_superclass_slot: bool) -> Vec<Spelled> {
        let superclass = (leads_with_superclass_slot || !self.superclass.is_none())
            .then(|| self.superclass.clone());
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
    fn supertype_spellings_align_with_a_leading_superclass_slot() {
        // A generic class's emitted supertype list ALWAYS leads with the superclass position,
        // holding `kotlin/Any` when the class declared none. The spellings must reserve that slot
        // too, or the first interface's abbreviation lands on `kotlin/Any`.
        let marker = Spelled::of_alias(crate::types::type_name("app/Marker"));
        let header = DeclaredSpellings {
            superclass: Spelled::default(),
            supertypes: vec![marker.clone()],
            ..DeclaredSpellings::default()
        };
        let generic = header.supertype_spellings(true);
        assert_eq!(generic.len(), 2, "the superclass slot is reserved");
        assert!(
            generic[0].is_none(),
            "an undeclared superclass spells nothing"
        );
        assert_eq!(generic[1], marker, "the interface keeps its own spelling");

        // A non-generic class omits that position entirely when no superclass was declared.
        let plain = header.supertype_spellings(false);
        assert_eq!(plain, vec![marker], "no reserved slot, no shift");
    }

    #[test]
    fn a_declared_superclass_leads_both_shapes() {
        let base = Spelled::of_alias(crate::types::type_name("app/Super"));
        let header = DeclaredSpellings {
            superclass: base.clone(),
            supertypes: Vec::new(),
            ..DeclaredSpellings::default()
        };
        // A DECLARED superclass occupies the leading position under either shape.
        assert_eq!(header.supertype_spellings(true), vec![base.clone()]);
        assert_eq!(header.supertype_spellings(false), vec![base]);
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
