//! kotlinc's `EnhancedNullability` type attribute.
//!
//! FIR's Java signature enhancement (`AbstractSignatureParts.computeIndexedQualifiers`) gives every
//! position of a Java result the nullability qualifier the declarations it overrides fix there. A
//! flexible position (`T!`) that an overridden declaration fixes not-null becomes rigid and carries
//! the `EnhancedNullability` attribute: `java.util.HashMap.entrySet()`, read as
//! `MutableMap.entries: MutableSet<MutableMap.MutableEntry<K, V>>`, is a rigid set of rigid entries
//! of rigid `K` and `V`, each marked. The attribute is not part of the type's identity: a marked
//! `String` is a `String`. It records that Java produced the value and nothing checked it, so
//! `Fir2IrImplicitCastInserter.insertSpecialCast` guards such a value where it is committed to a
//! type that rejects `null`.
//!
//! The attribute travels with the type it marks. Substituting a type parameter by a marked type
//! argument keeps the mark (`ConeSubstitutor` combines the argument's attributes with the
//! parameter's), so `hashMap.entries.iterator().next()` is a marked entry and its `key` a marked
//! `K`; a type variable fixed from marked constraints is marked. [`TypeEnhancement`] is that
//! attribute's layout over one type's shape, kept beside the semantic [`Ty`] rather than inside it.

use crate::types::{Ty, TypeName};

/// Which positions of one type carry `EnhancedNullability`: the type itself (`head`) and, in the
/// type's own argument order, each of its type arguments. Positions with no marked descendant are
/// left out, so an unmarked type is [`TypeEnhancement::NONE`] whatever its shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeEnhancement {
    head: bool,
    arguments: Vec<TypeEnhancement>,
}

impl TypeEnhancement {
    pub const NONE: TypeEnhancement = TypeEnhancement {
        head: false,
        arguments: Vec::new(),
    };

    pub fn new(head: bool, arguments: Vec<TypeEnhancement>) -> Self {
        let arguments = if arguments.iter().all(TypeEnhancement::is_none) {
            Vec::new()
        } else {
            arguments
        };
        TypeEnhancement { head, arguments }
    }

    pub fn is_none(&self) -> bool {
        !self.head && self.arguments.is_empty()
    }

    /// Whether the type itself carries the attribute.
    pub fn head(&self) -> bool {
        self.head
    }

    /// Whether any type argument carries the attribute somewhere.
    pub fn has_marked_arguments(&self) -> bool {
        !self.arguments.is_empty()
    }

    /// The layout of the `index`th type argument.
    pub fn argument(&self, index: usize) -> &TypeEnhancement {
        static NONE: TypeEnhancement = TypeEnhancement::NONE;
        self.arguments.get(index).unwrap_or(&NONE)
    }

    /// The same layout with the head marked as well when `head` is.
    pub fn with_head(&self, head: bool) -> Self {
        TypeEnhancement {
            head: self.head || head,
            arguments: self.arguments.clone(),
        }
    }

    /// The layout of a declaration whose type is inferred from a value of this layout. The
    /// declaration's type is the value's type without the attribute on its head; its arguments keep
    /// theirs (`val entries = hashMap.entries` is a set of marked entries).
    pub fn declared(&self) -> Self {
        TypeEnhancement::new(false, self.arguments.clone())
    }

    /// The layout a type variable is fixed to from several constraints: a position is marked only
    /// where every constraint marks it, as kotlinc's common supertype of a marked and an unmarked
    /// type is unmarked.
    pub fn meet(&self, other: &TypeEnhancement) -> Self {
        let count = self.arguments.len().min(other.arguments.len());
        TypeEnhancement::new(
            self.head && other.head,
            (0..count)
                .map(|index| self.arguments[index].meet(&other.arguments[index]))
                .collect(),
        )
    }

    /// Qualifiers contributed by independent overridden declarations at the same indexed type
    /// positions. A position is enhanced when any exact override fixes it not-null.
    pub fn union(&self, other: &TypeEnhancement) -> Self {
        let count = self.arguments.len().max(other.arguments.len());
        TypeEnhancement::new(
            self.head || other.head,
            (0..count)
                .map(|index| self.argument(index).union(other.argument(index)))
                .collect(),
        )
    }

    /// Substitute the type parameters of the `declared` type this layout describes:
    /// `binding(name)` is the layout of the type argument the parameter `name` is bound to, or
    /// `None` when it is not bound. A substituted parameter keeps its own mark and gains its
    /// argument's.
    pub fn substitute(
        &self,
        declared: Ty,
        binding: &dyn Fn(&str) -> Option<TypeEnhancement>,
    ) -> TypeEnhancement {
        match declared {
            Ty::TyParam(name, _) => match binding(name) {
                Some(argument) => argument.with_head(self.head),
                None => self.clone(),
            },
            Ty::Nullable(inner)
            | Ty::PlatformNullable(inner)
            | Ty::DefinitelyNotNull(inner)
            | Ty::InProjection(inner)
            | Ty::OutProjection(inner) => self.substitute(*inner, binding),
            Ty::Obj(_, arguments) => TypeEnhancement::new(
                self.head,
                arguments
                    .iter()
                    .enumerate()
                    .map(|(index, &argument)| self.argument(index).substitute(argument, binding))
                    .collect(),
            ),
            _ => self.clone(),
        }
    }
}

/// A callable's result as kotlinc's attribute substitution reads it: the marks of its declared
/// result and, once a receiver has specialized a member, the result its declaring classifier states
/// over that classifier's own type parameters. A call's value takes the marks of the receiver's type
/// arguments through those parameters, which the specialized result no longer names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnhancedResult {
    /// The marks of the declared result.
    pub marks: TypeEnhancement,
    /// The declaring classifier and the result over its type parameters, before specialization.
    pub declared: Option<(TypeName, Ty)>,
}

impl EnhancedResult {
    pub const NONE: EnhancedResult = EnhancedResult {
        marks: TypeEnhancement::NONE,
        declared: None,
    };

    /// Record that a receiver of `classifier` specializes the declared `result`. Only the first
    /// specialization names the declaring classifier; a later one sees an already specialized
    /// result.
    pub fn specialize(&mut self, classifier: Option<TypeName>, result: Ty) {
        if self.declared.is_none() {
            self.declared = classifier.map(|classifier| (classifier, result));
        }
    }
}

/// Enhance a Java result `java` from the result `overridden` of a declaration it overrides, as
/// `computeIndexedQualifiers` does for a covariant position: each flexible position the overridden
/// declaration fixes not-null becomes rigid and marked. Returns the enhanced type and its layout.
///
/// A position is flexible when it is a platform type (`Map.Entry<K, V>!`) or a Java type-parameter
/// use, whose flexibility the provider keeps on its bound (`E` with bound `Any!`); the latter stays
/// the same parameter. The overridden position fixes not-null when it is neither nullable nor
/// flexible, a type parameter included (Kotlin's `E` in `MutableIterator<E>`). Positions the two
/// shapes do not share are left alone.
pub fn enhance_from_overridden(java: Ty, overridden: Ty) -> (Ty, TypeEnhancement) {
    let fixes_not_null = !matches!(
        overridden,
        Ty::Nullable(_) | Ty::PlatformNullable(_) | Ty::StarProjection(_) | Ty::Error
    );
    match java {
        Ty::PlatformNullable(inner) if fixes_not_null => {
            let (rigid, layout) = enhance_from_overridden(*inner, overridden);
            (rigid, layout.with_head(true))
        }
        Ty::PlatformNullable(inner) => {
            let (inner, layout) = enhance_from_overridden(*inner, overridden.non_null());
            (Ty::platform_nullable(inner), layout)
        }
        Ty::TyParam(_, bound) if fixes_not_null && matches!(bound, Ty::PlatformNullable(_)) => {
            (java, TypeEnhancement::new(true, Vec::new()))
        }
        Ty::InProjection(inner) | Ty::OutProjection(inner) => {
            let overridden = match overridden {
                Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
                other => other,
            };
            let (enhanced, layout) = enhance_from_overridden(*inner, overridden);
            let enhanced = match java {
                Ty::InProjection(_) => Ty::in_projection(enhanced),
                _ => Ty::out_projection(enhanced),
            };
            (enhanced, layout)
        }
        Ty::Obj(owner, arguments) => {
            let fixed = overridden.non_null().type_args();
            if fixed.len() != arguments.len() {
                return (java, TypeEnhancement::NONE);
            }
            let (enhanced, layouts): (Vec<Ty>, Vec<TypeEnhancement>) = arguments
                .iter()
                .zip(fixed)
                .map(|(&argument, &fixed)| enhance_from_overridden(argument, fixed))
                .unzip();
            (
                Ty::obj_args_name(owner, &enhanced),
                TypeEnhancement::new(false, layouts),
            )
        }
        _ => (java, TypeEnhancement::NONE),
    }
}

#[cfg(test)]
mod tests {
    use super::{enhance_from_overridden, TypeEnhancement};
    use crate::types::Ty;

    fn any() -> Ty {
        Ty::nullable(Ty::obj("kotlin/Any"))
    }

    fn java_parameter(name: &'static str) -> Ty {
        Ty::ty_param(name, Ty::platform_nullable(Ty::obj("kotlin/Any")))
    }

    fn marked() -> TypeEnhancement {
        TypeEnhancement::new(true, Vec::new())
    }

    #[test]
    fn every_flexible_position_a_builtin_fixes_not_null_is_rigid_and_marked() {
        let entry = |key: Ty, value: Ty| {
            Ty::obj_args("kotlin/collections/MutableMap.MutableEntry", &[key, value])
        };
        let set = |element: Ty| Ty::obj_args("kotlin/collections/MutableSet", &[element]);
        let java = Ty::platform_nullable(set(Ty::platform_nullable(entry(
            java_parameter("K"),
            java_parameter("V"),
        ))));
        let overridden = set(entry(Ty::ty_param("K", any()), Ty::ty_param("V", any())));
        let (enhanced, layout) = enhance_from_overridden(java, overridden);
        assert_eq!(
            enhanced,
            set(entry(java_parameter("K"), java_parameter("V"))),
            "the set and its entries are rigid; the Java type parameters stay themselves"
        );
        assert_eq!(
            layout,
            TypeEnhancement::new(
                true,
                vec![TypeEnhancement::new(true, vec![marked(), marked()])]
            )
        );
    }

    #[test]
    fn a_nullable_overridden_position_leaves_the_java_position_flexible_and_unmarked() {
        let list = |element: Ty| Ty::obj_args("kotlin/collections/MutableList", &[element]);
        let java = Ty::platform_nullable(list(Ty::platform_nullable(Ty::String)));
        let (enhanced, layout) = enhance_from_overridden(java, list(Ty::nullable(Ty::String)));
        assert_eq!(enhanced, list(Ty::platform_nullable(Ty::String)));
        assert_eq!(layout, marked());
    }

    #[test]
    fn substitution_carries_an_argument_mark_into_the_parameter_it_binds() {
        let iterator = Ty::obj_args(
            "kotlin/collections/MutableIterator",
            &[Ty::ty_param("E", any())],
        );
        let declared = TypeEnhancement::new(true, Vec::new());
        let entry = TypeEnhancement::new(true, vec![marked(), marked()]);
        let substituted =
            declared.substitute(iterator, &|name| (name == "E").then(|| entry.clone()));
        assert_eq!(substituted, TypeEnhancement::new(true, vec![entry.clone()]));
        assert_eq!(
            TypeEnhancement::NONE.substitute(Ty::ty_param("E", any()), &|_| Some(entry.clone())),
            entry
        );
    }

    #[test]
    fn a_type_variable_is_marked_only_where_every_constraint_marks_it() {
        let set = TypeEnhancement::new(true, vec![marked()]);
        assert_eq!(set.meet(&set), set);
        assert_eq!(set.meet(&TypeEnhancement::NONE), TypeEnhancement::NONE);
        assert_eq!(
            set.meet(&TypeEnhancement::new(false, vec![marked()])),
            TypeEnhancement::new(false, vec![marked()])
        );
    }

    #[test]
    fn independent_override_qualifiers_are_unioned_by_type_position() {
        let left = TypeEnhancement::new(true, vec![marked(), TypeEnhancement::NONE]);
        let right = TypeEnhancement::new(false, vec![TypeEnhancement::NONE, marked()]);
        let expected = TypeEnhancement::new(true, vec![marked(), marked()]);
        assert_eq!(left.union(&right), expected);
        assert_eq!(
            right.union(&left),
            expected,
            "supertype order is irrelevant"
        );
    }
}
