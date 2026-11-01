//! Types and signatures proven publishable at the Pass-1 boundary.

use crate::types::Ty;

/// A type proven suitable for publication. The field is private: pending/error types cannot be
/// manufactured by users of the resolved module index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTy(Ty);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnpublishableType {
    Pending,
    Error,
}

impl ResolvedTy {
    pub fn new(ty: Ty) -> Result<Self, UnpublishableType> {
        let ty = ty.canonical_semantic();
        if ty.mentions_pending() {
            Err(UnpublishableType::Pending)
        } else if ty.mentions_error() {
            Err(UnpublishableType::Error)
        } else {
            Ok(Self(ty))
        }
    }

    pub const fn get(self) -> Ty {
        self.0
    }
}

/// The declaration's exposed result and any distinct checking context for its own inferred body.
/// Both views have removed solver-local parameters; only the exposed view approximates anonymous
/// implementation types. Callers, metadata and ABI consume `published` exclusively.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationResultTypes {
    pub published: ResolvedTy,
    pub inferred_body_result: Option<ResolvedTy>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSignature {
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    /// Declaration-owned body evidence before anonymous exposure, with solver-local parameters
    /// removed. This is checking context, never a caller-visible result or emitted ABI slot.
    pub(super) inferred_body_result: Option<ResolvedTy>,
    /// The typealias an inferred result was reached through, kotlinc's abbreviation attribute on
    /// the result type (`val text = StringBuilder()` has type `java.lang.StringBuilder`
    /// abbreviated as `kotlin.text.StringBuilder`). It never takes part in type identity: only
    /// `@Metadata` reads it, as the declaration's `abbreviatedType`.
    pub result_abbreviation: Option<ResolvedTypeAbbreviation>,
    /// The typealias application an explicit result is written as (`fun names(): Names`). A
    /// call's result carries it, so a declaration inferred from that call records it as well.
    pub declared_result_alias: Option<crate::libraries::DeclaredResultAlias>,
}

/// A typealias application that abbreviates a resolved type: the alias's qualified identity and
/// its type arguments, in the alias's own parameter order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTypeAbbreviation {
    pub alias: crate::types::TypeName,
    pub arguments: Box<[Ty]>,
    /// Expanded-type argument indices from the inferred result root to the node abbreviated by
    /// `alias`. Constructor aliases abbreviate the root and therefore carry an empty path; a
    /// class literal through `Alias<T>` abbreviates the represented `KClass` argument at `[0]`.
    pub argument_path: Box<[u32]>,
    /// The alias use is marked `?` (`Names?`, `Foo<T>?`). An alias whose own expansion is
    /// nullable (`typealias MaybeNames = Names?`) abbreviates a nullable type unmarked.
    pub marked_nullable: bool,
}

impl ResolvedTypeAbbreviation {
    /// An alias application that abbreviates the result root itself.
    pub fn root(
        alias: crate::types::TypeName,
        arguments: Box<[Ty]>,
        marked_nullable: bool,
    ) -> Self {
        Self {
            alias,
            arguments,
            argument_path: Box::new([]),
            marked_nullable,
        }
    }
}

impl ResolvedSignature {
    pub(crate) fn inferred_body_result(&self) -> Option<ResolvedTy> {
        self.inferred_body_result
    }

    /// The typealias application the result is spelled through at its root: the explicit
    /// result's alias, or the abbreviation an inferred result was reached through.
    pub(crate) fn result_alias(&self) -> Option<crate::libraries::DeclaredResultAlias> {
        self.result_abbreviation
            .as_ref()
            .filter(|abbreviation| abbreviation.argument_path.is_empty())
            .map(|abbreviation| {
                crate::libraries::DeclaredResultAlias::new(
                    abbreviation.alias,
                    &abbreviation.arguments,
                    abbreviation.marked_nullable,
                )
            })
            .or(self.declared_result_alias)
    }

    pub fn new(
        parameters: impl IntoIterator<Item = Ty>,
        result: Ty,
    ) -> Result<Self, UnpublishableType> {
        let parameters = parameters
            .into_iter()
            .map(ResolvedTy::new)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        Ok(Self {
            parameters,
            result: ResolvedTy::new(result)?,
            inferred_body_result: None,
            result_abbreviation: None,
            declared_result_alias: None,
        })
    }
}
