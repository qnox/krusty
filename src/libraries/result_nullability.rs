//! A callable's result nullability as its declaration states it.

use crate::types::Ty;

#[derive(Clone, Copy, Default)]
pub struct ReturnInfo {
    pub nullable: bool,
    pub class: Option<Ty>,
}

impl ReturnInfo {
    pub fn new(nullable: bool, class: Option<Ty>) -> Self {
        ReturnInfo { nullable, class }
    }

    pub fn apply(self, fallback: Ty) -> Ty {
        self.apply_with_class(self.class, fallback)
    }

    pub fn apply_with_class(self, class: Option<Ty>, fallback: Ty) -> Ty {
        let ret = match class {
            // Nullability wraps the declared generic result; it must not hide the already-solved
            // type arguments. Otherwise a dependency `Box<T>?` specialized as `Box<Base>?` is
            // collapsed back to raw `Box?` while the equivalent source declaration stays precise.
            Some(meta) if !fallback.non_null().type_args().is_empty() => {
                let specialized = Ty::obj_args(&meta.name(), fallback.non_null().type_args());
                if matches!(meta, Ty::PlatformNullable(_)) {
                    Ty::platform_nullable(specialized)
                } else {
                    specialized
                }
            }
            Some(meta) => meta,
            None => fallback,
        };
        if self.nullable && !ret.is_nullable() && (ret.boxed_ref().is_some() || ret.is_reference())
        {
            Ty::nullable(ret)
        } else {
            ret
        }
    }
}

/// kotlinc's Java signature enhancement of a callable's result nullability (`FirSignatureEnhancement`
/// with `computeQualifiersForOverride`).
///
/// A Java result with no nullability qualifier is flexible (`T!`). It becomes not-null when the
/// declaration says so itself (`@NotNull`), or when a declaration it overrides fixes the result
/// not-null: `StringBuilder.toString()` overrides `Any.toString(): String`. kotlinc marks such a
/// result with its `EnhancedNullability` attribute; the value is typed not-null, but nothing has
/// checked it, so committing it to a position whose declared type rejects `null` is guarded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultEnhancement {
    /// The declared result's nullability is the declaration's own.
    #[default]
    None,
    /// A JDK method realizing a mapped Kotlin builtin classifier (`java.lang.Throwable` for
    /// `kotlin.Throwable`). Kotlin's scope shows the builtin declaration such a method overrides,
    /// so an override gives it that declaration's result, not an enhanced one.
    MappedRealization,
    /// A flexible Java result (`T!`): the result an override enhancement makes it, which is the
    /// declared result without its head flexibility, specialized like the declared one.
    Flexible(Ty),
    /// [`Self::Flexible`] for a [`Self::MappedRealization`].
    MappedFlexible(Ty),
    /// The result is enhanced to not-null.
    NotNull,
}
