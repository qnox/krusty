//! A declaration's result type as written through a typealias.

use crate::types::{Ty, TyKind, TypeName};

/// The typealias a declaration spelled its result type through, applied to its as-written
/// arguments in the declaration's own type parameters (`val foo: Foo<T>?` is `Foo<T>?`). The
/// application is nullable when the use marked the alias `?`.
///
/// kotlinc keeps this abbreviation on the type a call or read of the declaration produces, with
/// the arguments substituted the way the selection substitutes the declaration's result, so a
/// declaration whose type is inferred from that call or read records it in its own metadata. An
/// argument the expansion never mentions (`typealias Foo<T> = List<String>`) still travels here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeclaredResultAlias(Ty);

impl DeclaredResultAlias {
    pub fn new(alias: TypeName, arguments: &[Ty], marked_nullable: bool) -> Self {
        let application = Ty::obj_args_name(alias, arguments);
        Self(if marked_nullable {
            Ty::nullable(application)
        } else {
            application
        })
    }

    /// The alias application a provider decoded as a type: the alias as its classifier, `?` when
    /// marked nullable.
    pub fn from_application(application: Ty) -> Option<Self> {
        matches!(application.non_null().kind(), TyKind::Obj(..)).then_some(Self(application))
    }

    pub fn alias(self) -> TypeName {
        match self.0.non_null().kind() {
            TyKind::Obj(alias, _) => alias,
            _ => unreachable!("an alias application is a classifier application"),
        }
    }

    pub fn arguments(self) -> Box<[Ty]> {
        self.0.non_null().type_args().into()
    }

    /// The application mentions none of the declaration's type parameters, so it needs no
    /// substitution at a selection.
    pub fn is_closed(self) -> bool {
        !self.0.mentions_ty_param()
    }

    pub fn marked_nullable(self) -> bool {
        self.0.is_nullable()
    }

    /// The application with its arguments substituted by `substitute`, the operation that
    /// specializes the declaration's result at the selection.
    pub fn substituted(self, substitute: impl FnOnce(Ty) -> Ty) -> Self {
        let substituted = substitute(self.0.non_null());
        Self::new(
            match substituted.non_null().kind() {
                TyKind::Obj(alias, _) => alias,
                _ => return self,
            },
            substituted.non_null().type_args(),
            self.marked_nullable(),
        )
    }
}
