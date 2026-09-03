//! Streaming frontend ownership model.
//!
//! This module is the migration boundary between parser-owned syntax and checked FIR.  It contains
//! only data structures: stable module identities, source origins, the temporary packed signature
//! graph, and the pending-free signature index.  Signature evaluation deliberately does not live
//! here; it must call the ordinary resolver/checker instead of growing a second typer.

use crate::ast::{
    ClassDecl, ClassInit, Decl, DeclId, Expr, File, FunBody, FunDecl, Param, PropDecl, PropParam,
    TrFlags, TypeAliasDecl, TypeRef,
};
use crate::diag::{DiagSink, Severity, Span};
use crate::features::LangFeatures;
use crate::source::{SourceInput, SourceKind};
use crate::types::Visibility;

pub use super::declaration_stub::*;
pub use super::identities::*;
pub use super::lookup_scope::*;
use super::signature::InferredSignatureKind;

fn companion_declarations(file: &File) -> std::collections::HashSet<DeclId> {
    file.decls
        .iter()
        .filter_map(|declaration| match file.decl(*declaration) {
            Decl::Class(class) => class.companion,
            Decl::Fun(_) | Decl::Property(_) => None,
        })
        .collect()
}

fn classifier_identity(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    declaration: DeclId,
) -> Option<DeclarationId> {
    let Decl::Class(class) = file.decl(declaration) else {
        return None;
    };
    let owner = file
        .decls
        .iter()
        .copied()
        .filter(|candidate| *candidate != declaration && !file.is_local_declaration(*candidate))
        .filter_map(|candidate| match file.decl(candidate) {
            Decl::Class(candidate_class)
                if candidate_class.span.lo < class.span.lo
                    && class.span.hi < candidate_class.span.hi =>
            {
                Some((candidate_class.span.hi - candidate_class.span.lo, candidate))
            }
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .min_by_key(|(length, _)| *length)
        .and_then(|(_, owner)| classifier_identity(file, source, ids, owner));
    let companions = companion_declarations(file);
    let sibling = if companions.contains(&declaration) {
        0
    } else {
        u32::try_from(
            file.decls
                .iter()
                .position(|candidate| *candidate == declaration)?,
        )
        .ok()?
    };
    Some(ids.intern(DeclarationAnchor {
        source,
        range: class.span,
        owner,
        kind: DeclarationKind::Classifier,
        sibling,
    }))
}

fn anonymous_property_owner(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    property: &PropDecl,
    owner: Option<DeclarationId>,
    sibling: u32,
    anonymous: Span,
) -> Option<(u32, DeclarationId)> {
    let contains = |outer: Span| outer.lo <= anonymous.lo && anonymous.hi <= outer.hi;
    let body_range = |body: &FunBody| match body {
        FunBody::Expr(expression) | FunBody::Block(expression) => file.expr_span(*expression),
        FunBody::None => None,
    };
    if let Some(range) = property
        .getter
        .as_ref()
        .and_then(body_range)
        .filter(|range| contains(*range))
    {
        let property_id = ids.intern(DeclarationAnchor {
            source,
            range: property.span,
            owner,
            kind: DeclarationKind::Property,
            sibling,
        });
        return Some((
            range.hi - range.lo,
            ids.intern(DeclarationAnchor {
                source,
                range,
                owner: Some(property_id),
                kind: DeclarationKind::Accessor,
                sibling: 0,
            }),
        ));
    }
    if let Some(range) = property
        .setter
        .as_ref()
        .and_then(|setter| setter.body.as_ref())
        .and_then(body_range)
        .filter(|range| contains(*range))
    {
        let property_id = ids.intern(DeclarationAnchor {
            source,
            range: property.span,
            owner,
            kind: DeclarationKind::Property,
            sibling,
        });
        return Some((
            range.hi - range.lo,
            ids.intern(DeclarationAnchor {
                source,
                range,
                owner: Some(property_id),
                kind: DeclarationKind::Accessor,
                sibling: 1,
            }),
        ));
    }
    property
        .init
        .into_iter()
        .chain(property.delegate)
        .filter_map(|expression| file.expr_span(expression))
        .filter(|range| contains(*range))
        .min_by_key(|range| range.hi - range.lo)
        .map(|range| {
            let property_id = ids.intern(DeclarationAnchor {
                source,
                range: property.span,
                owner,
                kind: DeclarationKind::Property,
                sibling,
            });
            (range.hi - range.lo, property_id)
        })
}

fn property_contains_anonymous(file: &File, property: &PropDecl, anonymous: Span) -> bool {
    let contains = |outer: Span| outer.lo <= anonymous.lo && anonymous.hi <= outer.hi;
    let body_range = |body: &FunBody| match body {
        FunBody::Expr(expression) | FunBody::Block(expression) => file.expr_span(*expression),
        FunBody::None => None,
    };
    property
        .getter
        .as_ref()
        .and_then(body_range)
        .is_some_and(contains)
        || property
            .setter
            .as_ref()
            .and_then(|setter| setter.body.as_ref())
            .and_then(body_range)
            .is_some_and(contains)
        || property
            .init
            .into_iter()
            .chain(property.delegate)
            .filter_map(|expression| file.expr_span(expression))
            .any(contains)
}

fn local_executable_owner(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    declaration: DeclId,
    classifier_ids: &std::collections::HashMap<DeclId, DeclarationId>,
) -> Option<DeclarationId> {
    let local = match file.decl(declaration) {
        Decl::Class(class) => class.span,
        Decl::Fun(_) | Decl::Property(_) => return None,
    };
    let mut property_candidates = Vec::new();
    for (sibling, candidate) in file.decls.iter().copied().enumerate() {
        match file.decl(candidate) {
            Decl::Property(property) => {
                if let Some(candidate) = anonymous_property_owner(
                    file,
                    source,
                    ids,
                    property,
                    None,
                    u32::try_from(sibling).expect("too many file declarations"),
                    local,
                ) {
                    property_candidates.push(candidate);
                }
            }
            Decl::Class(class) => {
                if !class
                    .body_props
                    .iter()
                    .any(|property| property_contains_anonymous(file, property, local))
                {
                    continue;
                }
                let class_id = classifier_ids
                    .get(&candidate)
                    .copied()
                    .or_else(|| classifier_identity(file, source, ids, candidate));
                let Some(class_id) = class_id else {
                    continue;
                };
                for (property_sibling, property) in class.body_props.iter().enumerate() {
                    if let Some(candidate) = anonymous_property_owner(
                        file,
                        source,
                        ids,
                        property,
                        Some(class_id),
                        u32::try_from(property_sibling).expect("too many class properties"),
                        local,
                    ) {
                        property_candidates.push(candidate);
                    }
                }
            }
            Decl::Fun(_) => {}
        }
    }
    if let Some((_, owner)) = property_candidates
        .into_iter()
        .min_by_key(|(length, _)| *length)
    {
        return Some(owner);
    }

    let contains = |outer: Span| outer.lo <= local.lo && local.hi <= outer.hi;
    let mut function_candidates = Vec::new();
    for (sibling, candidate) in file.decls.iter().copied().enumerate() {
        match file.decl(candidate) {
            Decl::Fun(function) if contains(function.span) => {
                function_candidates.push((
                    function.span.hi - function.span.lo,
                    ids.intern(DeclarationAnchor {
                        source,
                        range: function.span,
                        owner: None,
                        kind: DeclarationKind::Function,
                        sibling: u32::try_from(sibling).expect("too many file declarations"),
                    }),
                ));
            }
            Decl::Class(class) => {
                let containing_methods = class
                    .methods
                    .iter()
                    .enumerate()
                    .filter(|(_, function)| contains(function.span))
                    .collect::<Vec<_>>();
                if containing_methods.is_empty() {
                    continue;
                }
                let class_id = classifier_ids
                    .get(&candidate)
                    .copied()
                    .or_else(|| classifier_identity(file, source, ids, candidate));
                let Some(class_id) = class_id else {
                    continue;
                };
                for (method_sibling, function) in containing_methods {
                    function_candidates.push((
                        function.span.hi - function.span.lo,
                        ids.intern(DeclarationAnchor {
                            source,
                            range: function.span,
                            owner: Some(class_id),
                            kind: DeclarationKind::Function,
                            sibling: u32::try_from(method_sibling).expect("too many class methods"),
                        }),
                    ));
                }
            }
            Decl::Fun(_) | Decl::Property(_) => {}
        }
    }
    if let Some((_, owner)) = function_candidates
        .into_iter()
        .min_by_key(|(length, _)| *length)
    {
        return Some(owner);
    }

    match file
        .anonymous_object_enclosing_functions
        .get(&declaration)?
    {
        crate::ast::AnonymousEnclosingFunction::TopLevel(function) => {
            let Decl::Fun(function_decl) = file.decl(*function) else {
                return None;
            };
            let sibling = u32::try_from(
                file.decls
                    .iter()
                    .position(|candidate| candidate == function)?,
            )
            .ok()?;
            Some(ids.intern(DeclarationAnchor {
                source,
                range: function_decl.span,
                owner: None,
                kind: DeclarationKind::Function,
                sibling,
            }))
        }
        crate::ast::AnonymousEnclosingFunction::Member { class, method } => {
            let owner = classifier_ids
                .get(class)
                .copied()
                .or_else(|| classifier_identity(file, source, ids, *class))?;
            let Decl::Class(class_decl) = file.decl(*class) else {
                return None;
            };
            let function = class_decl.methods.get(*method as usize)?;
            Some(ids.intern(DeclarationAnchor {
                source,
                range: function.span,
                owner: Some(owner),
                kind: DeclarationKind::Function,
                sibling: *method,
            }))
        }
    }
}

fn nested_classifier_owners(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
) -> std::collections::HashMap<DeclId, DeclarationId> {
    let mut owners = std::collections::HashMap::new();
    let mut local_declarations = file
        .local_class_decls
        .values()
        .copied()
        .chain(file.local_class_nested.values().flatten().copied())
        .collect::<Vec<_>>();
    local_declarations.sort_unstable_by_key(|declaration| match file.decl(*declaration) {
        Decl::Class(class) => (
            usize::MAX - (class.span.hi - class.span.lo) as usize,
            class.span.lo,
        ),
        Decl::Fun(_) | Decl::Property(_) => (usize::MAX, u32::MAX),
    });
    local_declarations.dedup();
    let mut stable_local = std::collections::HashMap::new();
    for declaration in local_declarations.iter().copied() {
        let Decl::Class(class) = file.decl(declaration) else {
            continue;
        };
        // An anonymous classifier belongs to the executable declaration that contains its
        // construction, even when another local classifier also contains its source range. Use the
        // already-interned classifier identity so the member-function owner is canonical rather
        // than an anchor-only duplicate.
        let executable_owner =
            local_executable_owner(file, source, ids, declaration, &stable_local);
        let classifier_owner = local_declarations
            .iter()
            .copied()
            .filter(|candidate| *candidate != declaration)
            .filter_map(|candidate| match file.decl(candidate) {
                Decl::Class(candidate_class)
                    if candidate_class.span.lo <= class.span.lo
                        && class.span.hi <= candidate_class.span.hi =>
                {
                    Some((candidate_class.span.hi - candidate_class.span.lo, candidate))
                }
                Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
            })
            .min_by_key(|(length, _)| *length)
            .and_then(|(_, owner)| stable_local.get(&owner).copied());
        // A classifier declared as a member of a local classifier is parser-hoisted into
        // `file.decls`, but its semantic owner remains that classifier. Statement-local and
        // anonymous classifiers instead belong to the executable declaration that introduces
        // them, even when their source range is nested inside a classifier declaration.
        let nested_local_member = file
            .local_class_nested
            .values()
            .flatten()
            .any(|nested| *nested == declaration);
        let owner = if nested_local_member {
            classifier_owner.or(executable_owner)
        } else {
            executable_owner.or(classifier_owner)
        };
        let sibling = file
            .decls
            .iter()
            .position(|candidate| *candidate == declaration)
            .and_then(|position| u32::try_from(position).ok())
            .expect("a hoisted local classifier must be a file declaration");
        let id = ids.intern(DeclarationAnchor {
            source,
            range: class.span,
            owner,
            kind: DeclarationKind::Classifier,
            sibling,
        });
        stable_local.insert(declaration, id);
        if let Some(owner) = owner {
            owners.insert(declaration, owner);
        }
    }

    // Parser-hoisted member classifiers are also separate `file.decls` entries. Recover their
    // lexical ownership structurally from source containment, including companions, before either
    // compact-header walk interns an anchor. Local classifiers are handled above: their root belongs
    // to an executable body rather than becoming a member of the surrounding source class.
    let companions = companion_declarations(file);
    let mut declarations = file
        .decls
        .iter()
        .copied()
        .filter(|declaration| !file.is_local_declaration(*declaration))
        .filter(|declaration| matches!(file.decl(*declaration), Decl::Class(_)))
        .collect::<Vec<_>>();
    declarations.sort_by_key(|declaration| match file.decl(*declaration) {
        Decl::Class(class) => (
            usize::MAX - (class.span.hi - class.span.lo) as usize,
            class.span.lo,
        ),
        Decl::Fun(_) | Decl::Property(_) => unreachable!("filtered to classifiers"),
    });
    let mut stable = std::collections::HashMap::new();
    for declaration in declarations.iter().copied() {
        let Decl::Class(class) = file.decl(declaration) else {
            unreachable!("filtered to classifiers")
        };
        // An enum-entry body is a real semantic ownership boundary even though the parser does not
        // materialize its anonymous subclass as a `Decl::Class`. Consume the parser's transient
        // structural edge and immediately replace it with stable classifier/entry identities.
        let enum_entry_owner = file
            .enum_entry_nested_classifier_owners
            .get(&declaration)
            .and_then(|entry_range| {
                declarations.iter().copied().find_map(|candidate| {
                    let parent = stable.get(&candidate).copied()?;
                    let Decl::Class(candidate_class) = file.decl(candidate) else {
                        return None;
                    };
                    let (index, entry) = candidate_class
                        .enum_entries
                        .iter()
                        .enumerate()
                        .find(|(_, entry)| entry.span == *entry_range)?;
                    Some(ids.intern(DeclarationAnchor {
                        source,
                        range: entry.span,
                        owner: Some(parent),
                        kind: DeclarationKind::EnumEntry,
                        sibling: u32::try_from(index).expect("too many enum entries"),
                    }))
                })
            });
        let classifier_owner = declarations
            .iter()
            .copied()
            .filter(|candidate| *candidate != declaration)
            .filter_map(|candidate| match file.decl(candidate) {
                Decl::Class(candidate_class)
                    if candidate_class.span.lo < class.span.lo
                        && class.span.hi < candidate_class.span.hi =>
                {
                    Some((candidate_class.span.hi - candidate_class.span.lo, candidate))
                }
                Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
            })
            .min_by_key(|(length, _)| *length)
            .and_then(|(_, owner)| stable.get(&owner).copied());
        // An anonymous classifier declared inside an executable belongs to that FUNCTION body,
        // even when source-span containment also places it inside the surrounding source class.
        // The executable edge is what lets Pass 1 identify anonymous declarations owned by an
        // inline body; choosing the wider class would falsely turn them into ordinary members.
        let anonymous_owner = file
            .is_anonymous_object_class(declaration)
            .then(|| local_executable_owner(file, source, ids, declaration, &stable))
            .flatten();
        let owner = enum_entry_owner.or(anonymous_owner).or(classifier_owner);
        let sibling = if companions.contains(&declaration) {
            0
        } else {
            file.decls
                .iter()
                .position(|candidate| *candidate == declaration)
                .and_then(|position| u32::try_from(position).ok())
                .expect("a hoisted classifier must be a file declaration")
        };
        let id = ids.intern(DeclarationAnchor {
            source,
            range: class.span,
            owner,
            kind: DeclarationKind::Classifier,
            sibling,
        });
        stable.insert(declaration, id);
        if let Some(owner) = owner {
            owners.insert(declaration, owner);
        }
    }
    stable.extend(stable_local);
    for declaration in file.anonymous_object_classes.values().copied() {
        if let Some(owner) = local_executable_owner(file, source, ids, declaration, &stable) {
            owners.entry(declaration).or_insert(owner);
        }
    }
    owners
}
pub use super::source_map::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderTypeRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderParameterRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderTypeParameterRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderTypeBoundRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderInterfaceDelegationRange {
    start: u32,
    len: u32,
}

/// Every bit of parser-owned type syntax which affects semantic type resolution. This is a compact
/// copy, not a semantic type: it exists only while explicit headers are resolved and is discarded
/// before checked FIR streaming starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderTypeFlags(u8);

impl HeaderTypeFlags {
    const NULLABLE: u8 = 1 << 0;
    const DEFINITELY_NON_NULL: u8 = 1 << 1;
    const FUNCTION_RECEIVER: u8 = 1 << 2;
    const SUSPEND_FUNCTION: u8 = 1 << 3;
    const IN_PROJECTION: u8 = 1 << 4;
    const OUT_PROJECTION: u8 = 1 << 5;
    const IMPORT: u8 = 1 << 6;
    const STAR_PROJECTION: u8 = 1 << 7;

    fn from_type_ref(ty: &TypeRef) -> Self {
        let mut bits = 0;
        for (flag, enabled) in [
            (Self::NULLABLE, ty.nullable()),
            (Self::DEFINITELY_NON_NULL, ty.definitely_non_null()),
            (Self::FUNCTION_RECEIVER, ty.fun_has_receiver()),
            (Self::SUSPEND_FUNCTION, ty.fun_suspend()),
            (Self::IN_PROJECTION, ty.in_projection()),
            (Self::OUT_PROJECTION, ty.out_projection()),
            (Self::IMPORT, ty.is_import()),
            (Self::STAR_PROJECTION, ty.is_star_projection()),
        ] {
            if enabled {
                bits |= flag;
            }
        }
        Self(bits)
    }

    pub const fn nullable(self) -> bool {
        self.0 & Self::NULLABLE != 0
    }

    pub const fn definitely_non_null(self) -> bool {
        self.0 & Self::DEFINITELY_NON_NULL != 0
    }

    pub const fn function_receiver(self) -> bool {
        self.0 & Self::FUNCTION_RECEIVER != 0
    }

    pub const fn suspend_function(self) -> bool {
        self.0 & Self::SUSPEND_FUNCTION != 0
    }

    pub const fn in_projection(self) -> bool {
        self.0 & Self::IN_PROJECTION != 0
    }

    pub const fn out_projection(self) -> bool {
        self.0 & Self::OUT_PROJECTION != 0
    }

    pub const fn is_import(self) -> bool {
        self.0 & Self::IMPORT != 0
    }

    pub const fn star_projection(self) -> bool {
        self.0 & Self::STAR_PROJECTION != 0
    }
}

/// Allocation-free packed type node. Children live in shared `HeaderTypeArena` storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderTypeKind {
    Classifier {
        detail: HeaderClassifierTypeId,
        /// Kept to make the copy exhaustive if a parser producer uses `TypeRef::arg` outside
        /// function syntax. Valid current classifier syntax leaves it unset.
        abbreviated_argument: Option<HeaderTypeId>,
    },
    Function {
        parameters: HeaderTypeRange,
        result: Option<HeaderTypeId>,
        context_count: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderClassifierType {
    pub path: LookupNameRange,
    pub arguments: HeaderTypeRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderType {
    pub kind: HeaderTypeKind,
    pub flags: HeaderTypeFlags,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderParameterFlags(u8);

impl HeaderParameterFlags {
    const VARARG: u8 = 1 << 0;
    const DEFAULT: u8 = 1 << 1;
    const PROPERTY: u8 = 1 << 2;
    const MUTABLE_PROPERTY: u8 = 1 << 3;

    const fn with(mut self, flag: u8, enabled: bool) -> Self {
        if enabled {
            self.0 |= flag;
        }
        self
    }

    pub const fn is_vararg(self) -> bool {
        self.0 & Self::VARARG != 0
    }

    pub const fn has_default(self) -> bool {
        self.0 & Self::DEFAULT != 0
    }

    pub const fn is_property(self) -> bool {
        self.0 & Self::PROPERTY != 0
    }

    pub const fn is_mutable_property(self) -> bool {
        self.0 & Self::MUTABLE_PROPERTY != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderParameter {
    /// Lookup input only. Selected call arguments later use a stable callable and parameter ordinal.
    pub name: LookupNameId,
    pub ty: HeaderTypeId,
    pub flags: HeaderParameterFlags,
    pub span: Span,
    /// Declaration annotations attached to the value parameter.
    pub annotations: HeaderTypeRange,
    /// Annotations written on the parameter's declared type.
    pub type_annotations: HeaderTypeRange,
    /// Class-literal arguments attached to parameter annotations, keyed by annotation ordinal.
    /// These are bounded declaration-header paths such as `pkg.Type` from `pkg.Type::class`;
    /// no annotation expression or parser identity survives extraction.
    pub annotation_class_literals: HeaderParameterAnnotationClassLiteralRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderParameterAnnotationClassLiteral {
    pub annotation_ordinal: u32,
    pub classifier: LookupNameRange,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderParameterAnnotationClassLiteralRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderTypeParameterFlags(u8);

impl HeaderTypeParameterFlags {
    const IN: u8 = 1 << 0;
    const OUT: u8 = 1 << 1;
    const NON_NULL: u8 = 1 << 2;
    const REIFIED: u8 = 1 << 3;

    fn new(variance: crate::types::TypeVariance, non_null: bool, reified: bool) -> Self {
        let mut flags = Self::default();
        flags.0 |= match variance {
            crate::types::TypeVariance::Invariant => 0,
            crate::types::TypeVariance::In => Self::IN,
            crate::types::TypeVariance::Out => Self::OUT,
        };
        if non_null {
            flags.0 |= Self::NON_NULL;
        }
        if reified {
            flags.0 |= Self::REIFIED;
        }
        flags
    }

    pub(crate) fn from_semantics(
        variance: crate::types::TypeVariance,
        non_null: bool,
        reified: bool,
    ) -> Self {
        Self::new(variance, non_null, reified)
    }

    pub const fn is_in(self) -> bool {
        self.0 & Self::IN != 0
    }

    pub const fn is_out(self) -> bool {
        self.0 & Self::OUT != 0
    }

    pub const fn is_non_null(self) -> bool {
        self.0 & Self::NON_NULL != 0
    }

    pub const fn is_reified(self) -> bool {
        self.0 & Self::REIFIED != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderTypeParameter {
    pub name: LookupNameId,
    pub flags: HeaderTypeParameterFlags,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderTypeBound {
    pub parameter: LookupNameId,
    pub ty: HeaderTypeId,
}

/// The runtime value source for one interface-delegation edge while compact header syntax exists.
/// A direct source parameter is a declaration-header fact. Every other expression is checked in
/// Pass 2 as part of the primary-constructor body; the packed delegation ordinal identifies it
/// without retaining an AST coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderInterfaceDelegateSource {
    ConstructorParameter(u32),
    ConstructorBodyInitializer,
}

/// One class interface-delegation edge in compact source order. The supertype ordinal refers to the
/// neighboring packed classifier-header array; no source spelling survives signature finalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderInterfaceDelegation {
    pub supertype: u32,
    pub source: HeaderInterfaceDelegateSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderResultType {
    Explicit(HeaderTypeId),
    /// Block-body functions with no written return type are semantically `Unit` and need no graph.
    ImplicitUnit,
    /// Expression-body functions and expression-derived properties are attached to `SignatureGraph`.
    Inferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderDeclarationKind {
    Callable {
        receiver: Option<HeaderTypeId>,
        parameters: HeaderParameterRange,
        result: HeaderResultType,
        type_parameters: HeaderTypeParameterRange,
        bounds: HeaderTypeBoundRange,
        context_count: u32,
        /// Start of the declaration signature, used only to derive stable semantic identities for
        /// callable type parameters after the parser-owned declaration has been released.
        signature_start: u32,
        /// End of the declaration signature. Together with `signature_start`, this preserves the
        /// exact diagnostic origin without retaining the declaration or body AST.
        signature_end: u32,
    },
    Property {
        receiver: Option<HeaderTypeId>,
        context_parameters: HeaderParameterRange,
        declared_type: Option<HeaderTypeId>,
        getter_type: Option<HeaderTypeId>,
        backing_field_type: Option<HeaderTypeId>,
        type_parameters: HeaderTypeParameterRange,
        bounds: HeaderTypeBoundRange,
        mutable: bool,
    },
    Classifier {
        type_parameters: HeaderTypeParameterRange,
        /// Enclosing declaration parameters used by a parser-hoisted local/anonymous classifier.
        /// These bind existing semantic declarations; they are not formals owned by this classifier.
        lexical_type_parameter_captures: HeaderTypeParameterRange,
        bounds: HeaderTypeBoundRange,
        /// Declared interface/function supertypes in source order.
        supertypes: HeaderTypeRange,
        /// The one superclass constructor target, kept distinct from interfaces so later semantic
        /// graph construction never recovers class-vs-interface ownership from source spelling.
        base: Option<HeaderTypeId>,
        /// Class context parameters in source order. They are shared by every constructor and are
        /// implicit receivers of instance bodies.
        context_parameters: HeaderParameterRange,
        primary_parameters: HeaderParameterRange,
        delegations: HeaderInterfaceDelegationRange,
    },
    Constructor {
        context_parameters: HeaderParameterRange,
        parameters: HeaderParameterRange,
    },
    TypeAlias {
        type_parameters: HeaderTypeParameterRange,
        target: HeaderTypeId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderDeclaration {
    pub declaration: DeclarationId,
    /// Declaration annotations as compact classifier-reference types. Resolution consumes this
    /// temporary range during Pass 1 and publishes stable classifier identities.
    pub annotations: HeaderTypeRange,
    pub kind: HeaderDeclarationKind,
}

/// Temporary packed syntax required to resolve explicit declaration signatures after the source
/// file AST has been released. It has no expression ids, statement ids, declaration-arena ids, or
/// body syntax.
#[derive(Default)]
pub struct HeaderSyntaxArena {
    types: Vec<HeaderType>,
    /// As-written type syntax for nodes rewritten by the parser's same-file type-alias expansion.
    /// Both keys and values are compact header identities. The parser's span-keyed rewrite table is
    /// consumed while one source AST is live and never becomes cross-pass state.
    source_spellings: std::collections::HashMap<HeaderTypeId, HeaderTypeId>,
    classifier_types: Vec<HeaderClassifierType>,
    type_operands: Vec<HeaderTypeId>,
    path_segments: Vec<LookupNameId>,
    parameters: Vec<HeaderParameter>,
    parameter_annotation_class_literals: Vec<HeaderParameterAnnotationClassLiteral>,
    type_parameters: Vec<HeaderTypeParameter>,
    bounds: Vec<HeaderTypeBound>,
    interface_delegations: Vec<HeaderInterfaceDelegation>,
    declarations: Vec<Option<HeaderDeclaration>>,
}

impl HeaderSyntaxArena {
    fn range<T>(storage: &mut Vec<T>, values: impl IntoIterator<Item = T>) -> (u32, u32) {
        let start = next_id(storage.len(), "packed header operands");
        storage.extend(values);
        let end = next_id(storage.len(), "packed header operands");
        (start, end - start)
    }

    fn add_type_range(
        &mut self,
        values: impl IntoIterator<Item = HeaderTypeId>,
    ) -> HeaderTypeRange {
        let (start, len) = Self::range(&mut self.type_operands, values);
        HeaderTypeRange { start, len }
    }

    fn add_parameter_range(
        &mut self,
        values: impl IntoIterator<Item = HeaderParameter>,
    ) -> HeaderParameterRange {
        let (start, len) = Self::range(&mut self.parameters, values);
        HeaderParameterRange { start, len }
    }

    fn add_parameter_annotation_class_literal_range(
        &mut self,
        values: impl IntoIterator<Item = HeaderParameterAnnotationClassLiteral>,
    ) -> HeaderParameterAnnotationClassLiteralRange {
        let (start, len) = Self::range(&mut self.parameter_annotation_class_literals, values);
        HeaderParameterAnnotationClassLiteralRange { start, len }
    }

    fn add_type_parameter_range(
        &mut self,
        values: impl IntoIterator<Item = HeaderTypeParameter>,
    ) -> HeaderTypeParameterRange {
        let (start, len) = Self::range(&mut self.type_parameters, values);
        HeaderTypeParameterRange { start, len }
    }

    fn add_bound_range(
        &mut self,
        values: impl IntoIterator<Item = HeaderTypeBound>,
    ) -> HeaderTypeBoundRange {
        let (start, len) = Self::range(&mut self.bounds, values);
        HeaderTypeBoundRange { start, len }
    }

    fn add_interface_delegation_range(
        &mut self,
        values: impl IntoIterator<Item = HeaderInterfaceDelegation>,
    ) -> HeaderInterfaceDelegationRange {
        let (start, len) = Self::range(&mut self.interface_delegations, values);
        HeaderInterfaceDelegationRange { start, len }
    }

    fn add_path(&mut self, spelling: &str, names: &mut LookupNames) -> LookupNameRange {
        let segments = spelling
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .map(|segment| names.intern(segment))
            .collect::<Vec<_>>();
        let (start, len) = Self::range(&mut self.path_segments, segments);
        LookupNameRange { start, len }
    }

    fn push_type(&mut self, ty: HeaderType) -> HeaderTypeId {
        let id = HeaderTypeId::from_raw(next_id(self.types.len(), "header types"));
        self.types.push(ty);
        id
    }

    fn add_classifier_detail(
        &mut self,
        path: LookupNameRange,
        arguments: HeaderTypeRange,
    ) -> HeaderClassifierTypeId {
        let id = HeaderClassifierTypeId::from_raw(next_id(
            self.classifier_types.len(),
            "header classifier type details",
        ));
        self.classifier_types
            .push(HeaderClassifierType { path, arguments });
        id
    }

    pub(crate) fn add_type(&mut self, ty: &TypeRef, names: &mut LookupNames) -> HeaderTypeId {
        let flags = HeaderTypeFlags::from_type_ref(ty);
        let kind = if ty.name == "<fun>" {
            let parameters = ty
                .fun_params
                .iter()
                .map(|parameter| self.add_type(parameter, names))
                .collect::<Vec<_>>();
            let parameters = self.add_type_range(parameters);
            let result = ty.arg.as_deref().map(|result| self.add_type(result, names));
            HeaderTypeKind::Function {
                parameters,
                result,
                context_count: ty.fun_context_count,
            }
        } else {
            let arguments = ty
                .targs
                .iter()
                .map(|argument| self.add_type(argument, names))
                .collect::<Vec<_>>();
            let arguments = self.add_type_range(arguments);
            let abbreviated_argument = ty
                .arg
                .as_deref()
                .map(|argument| self.add_type(argument, names));
            let path = self.add_path(&ty.name, names);
            let detail = self.add_classifier_detail(path, arguments);
            HeaderTypeKind::Classifier {
                detail,
                abbreviated_argument,
            }
        };
        self.push_type(HeaderType {
            kind,
            flags,
            span: ty.span,
        })
    }

    /// Consume the parser's temporary alias-rewrite sidecar for the compact types added since
    /// `first_type`. Associations are immediately converted from source spans to compact type
    /// identities; later signature/metadata publication never needs the `File` table.
    fn capture_source_spellings(
        &mut self,
        first_type: usize,
        spellings: &std::collections::HashMap<Span, TypeRef>,
        names: &mut LookupNames,
    ) {
        let rewritten = self.types[first_type..]
            .iter()
            .enumerate()
            .filter_map(|(offset, ty)| {
                spellings.get(&ty.span).cloned().map(|spelling| {
                    (
                        HeaderTypeId::from_raw(
                            u32::try_from(first_type + offset).expect("too many header types"),
                        ),
                        spelling,
                    )
                })
            })
            .collect::<Vec<_>>();
        for (expanded, spelling) in rewritten {
            let spelling = self.add_type(&spelling, names);
            self.source_spellings.insert(expanded, spelling);
        }
    }

    fn add_classifier_type(
        &mut self,
        spelling: &str,
        arguments: &[TypeRef],
        span: Span,
        names: &mut LookupNames,
    ) -> HeaderTypeId {
        let arguments = arguments
            .iter()
            .map(|argument| self.add_type(argument, names))
            .collect::<Vec<_>>();
        let arguments = self.add_type_range(arguments);
        let path = self.add_path(spelling, names);
        let detail = self.add_classifier_detail(path, arguments);
        self.push_type(HeaderType {
            kind: HeaderTypeKind::Classifier {
                detail,
                abbreviated_argument: None,
            },
            flags: HeaderTypeFlags::default(),
            span,
        })
    }

    fn insert_declaration(&mut self, declaration: HeaderDeclaration) {
        let index = declaration.declaration.raw() as usize;
        if self.declarations.len() <= index {
            self.declarations.resize(index + 1, None);
        }
        assert!(
            self.declarations[index].replace(declaration).is_none(),
            "compact header syntax may be published only once per stable declaration"
        );
    }

    pub fn declaration(&self, id: DeclarationId) -> Option<HeaderDeclaration> {
        self.declarations.get(id.raw() as usize).copied().flatten()
    }

    /// Direct type roots owned by one declaration header. Children are reached through
    /// [`Self::ty`] and shared operand ranges; body/local type syntax is structurally absent.
    pub fn declaration_type_roots(&self, id: DeclarationId) -> Vec<HeaderTypeId> {
        fn add_parameters(
            arena: &HeaderSyntaxArena,
            roots: &mut Vec<HeaderTypeId>,
            range: HeaderParameterRange,
        ) {
            roots.extend(arena.parameters(range).iter().map(|parameter| parameter.ty));
        }
        fn add_bounds(
            arena: &HeaderSyntaxArena,
            roots: &mut Vec<HeaderTypeId>,
            range: HeaderTypeBoundRange,
        ) {
            roots.extend(arena.bounds(range).iter().map(|bound| bound.ty));
        }
        let Some(declaration) = self.declaration(id) else {
            return Vec::new();
        };
        let mut roots = Vec::new();
        match declaration.kind {
            HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                result,
                bounds,
                ..
            } => {
                roots.extend(receiver);
                add_parameters(self, &mut roots, parameters);
                if let HeaderResultType::Explicit(result) = result {
                    roots.push(result);
                }
                add_bounds(self, &mut roots, bounds);
            }
            HeaderDeclarationKind::Property {
                receiver,
                context_parameters,
                declared_type,
                getter_type,
                backing_field_type,
                bounds,
                ..
            } => {
                roots.extend(receiver);
                add_parameters(self, &mut roots, context_parameters);
                roots.extend(declared_type);
                roots.extend(getter_type);
                roots.extend(backing_field_type);
                add_bounds(self, &mut roots, bounds);
            }
            HeaderDeclarationKind::Classifier {
                bounds,
                supertypes,
                base,
                context_parameters,
                primary_parameters,
                ..
            } => {
                add_bounds(self, &mut roots, bounds);
                roots.extend(self.type_operands(supertypes));
                roots.extend(base);
                add_parameters(self, &mut roots, context_parameters);
                add_parameters(self, &mut roots, primary_parameters);
            }
            HeaderDeclarationKind::Constructor {
                context_parameters,
                parameters,
            } => {
                add_parameters(self, &mut roots, context_parameters);
                add_parameters(self, &mut roots, parameters)
            }
            HeaderDeclarationKind::TypeAlias { target, .. } => roots.push(target),
        }
        roots
    }

    pub fn ty(&self, id: HeaderTypeId) -> Option<HeaderType> {
        self.types.get(id.raw() as usize).copied()
    }

    /// Reconstruct one short-lived parser type for the existing resolver during migration. The
    /// caller must resolve and drop it immediately; this never reconstructs a declaration or body
    /// AST, and every child comes from the compact header arena.
    pub fn transient_type_ref(&self, id: HeaderTypeId, names: &LookupNames) -> Option<TypeRef> {
        let ty = self.ty(id)?;
        let flags = TrFlags::default()
            .with_nullable(ty.flags.nullable())
            .with_definitely_non_null(ty.flags.definitely_non_null())
            .with_fun_has_receiver(ty.flags.function_receiver())
            .with_fun_suspend(ty.flags.suspend_function())
            .with_in_projection(ty.flags.in_projection())
            .with_out_projection(ty.flags.out_projection())
            .with_import(ty.flags.is_import())
            .with_star_projection(ty.flags.star_projection());
        let (name, arg, targs, fun_params, fun_context_count) = match ty.kind {
            HeaderTypeKind::Classifier {
                detail,
                abbreviated_argument,
            } => {
                let detail = self.classifier_type(detail)?;
                let name = self
                    .type_path(detail.path)
                    .iter()
                    .map(|segment| names.get(*segment))
                    .collect::<Option<Vec<_>>>()?
                    .join(".");
                let mut targs = Vec::new();
                for argument in self.type_operands(detail.arguments) {
                    targs.push(self.transient_type_ref(*argument, names)?);
                }
                let arg = match abbreviated_argument {
                    Some(argument) => Some(Box::new(self.transient_type_ref(argument, names)?)),
                    None => None,
                };
                (name, arg, targs, Vec::new(), 0)
            }
            HeaderTypeKind::Function {
                parameters,
                result,
                context_count,
            } => {
                let mut fun_params = Vec::new();
                for parameter in self.type_operands(parameters) {
                    fun_params.push(self.transient_type_ref(*parameter, names)?);
                }
                let arg = match result {
                    Some(result) => Some(Box::new(self.transient_type_ref(result, names)?)),
                    None => None,
                };
                ("<fun>".into(), arg, Vec::new(), fun_params, context_count)
            }
        };
        Some(TypeRef {
            name,
            flags,
            arg,
            targs,
            span: ty.span,
            fun_params,
            fun_context_count,
        })
    }

    /// Reconstruct only the source-spelling substitutions reachable from one compact type root.
    /// This is a short-lived adapter for metadata publication during Pass 1; the returned map is
    /// dropped with the materialized `TypeRef` and never crosses into checked FIR.
    pub(crate) fn transient_source_spellings(
        &self,
        root: HeaderTypeId,
        names: &LookupNames,
    ) -> Option<std::collections::HashMap<Span, TypeRef>> {
        fn visit(
            arena: &HeaderSyntaxArena,
            id: HeaderTypeId,
            names: &LookupNames,
            out: &mut std::collections::HashMap<Span, TypeRef>,
        ) -> Option<()> {
            let ty = arena.ty(id)?;
            if let Some(spelling) = arena.source_spellings.get(&id).copied() {
                out.insert(ty.span, arena.transient_type_ref(spelling, names)?);
            }
            match ty.kind {
                HeaderTypeKind::Classifier {
                    detail,
                    abbreviated_argument,
                } => {
                    let detail = arena.classifier_type(detail)?;
                    for argument in arena.type_operands(detail.arguments) {
                        visit(arena, *argument, names, out)?;
                    }
                    if let Some(argument) = abbreviated_argument {
                        visit(arena, argument, names, out)?;
                    }
                }
                HeaderTypeKind::Function {
                    parameters, result, ..
                } => {
                    for parameter in arena.type_operands(parameters) {
                        visit(arena, *parameter, names, out)?;
                    }
                    if let Some(result) = result {
                        visit(arena, result, names, out)?;
                    }
                }
            }
            Some(())
        }

        let mut spellings = std::collections::HashMap::new();
        visit(self, root, names, &mut spellings)?;
        Some(spellings)
    }

    pub fn classifier_type(&self, id: HeaderClassifierTypeId) -> Option<HeaderClassifierType> {
        self.classifier_types.get(id.raw() as usize).copied()
    }

    pub fn type_operands(&self, range: HeaderTypeRange) -> &[HeaderTypeId] {
        let start = range.start as usize;
        &self.type_operands[start..start + range.len as usize]
    }

    pub fn type_path(&self, range: LookupNameRange) -> &[LookupNameId] {
        let start = range.start as usize;
        &self.path_segments[start..start + range.len as usize]
    }

    pub fn parameters(&self, range: HeaderParameterRange) -> &[HeaderParameter] {
        let start = range.start as usize;
        &self.parameters[start..start + range.len as usize]
    }

    pub fn parameter_annotation_class_literals(
        &self,
        range: HeaderParameterAnnotationClassLiteralRange,
    ) -> &[HeaderParameterAnnotationClassLiteral] {
        let start = range.start as usize;
        &self.parameter_annotation_class_literals[start..start + range.len as usize]
    }

    /// Mutable view of one declaration's packed parameters. Multiplatform actualization uses this
    /// to publish an expect parameter's default-presence fact on the callable that survives as the
    /// actual declaration. The expression remains expect-owned only while Pass-1 syntax is live;
    /// checked default FIR is then stored under the surviving actual callable.
    pub fn parameters_mut(&mut self, range: HeaderParameterRange) -> &mut [HeaderParameter] {
        let start = range.start as usize;
        &mut self.parameters[start..start + range.len as usize]
    }

    /// Publish default-presence facts selected by expect/actual matching.
    pub fn set_parameter_defaults(&mut self, range: HeaderParameterRange, defaults: &[bool]) {
        for (parameter, &has_default) in self.parameters_mut(range).iter_mut().zip(defaults) {
            if has_default {
                parameter.flags = parameter.flags.with(HeaderParameterFlags::DEFAULT, true);
            }
        }
    }

    pub fn type_parameters(&self, range: HeaderTypeParameterRange) -> &[HeaderTypeParameter] {
        let start = range.start as usize;
        &self.type_parameters[start..start + range.len as usize]
    }

    pub fn bounds(&self, range: HeaderTypeBoundRange) -> &[HeaderTypeBound] {
        let start = range.start as usize;
        &self.bounds[start..start + range.len as usize]
    }

    pub fn interface_delegations(
        &self,
        range: HeaderInterfaceDelegationRange,
    ) -> &[HeaderInterfaceDelegation] {
        let start = range.start as usize;
        &self.interface_delegations[start..start + range.len as usize]
    }

    pub fn type_count(&self) -> usize {
        self.types.len()
    }

    pub fn declaration_count(&self) -> usize {
        self.declarations.iter().flatten().count()
    }

    pub(crate) fn storage_payload_bytes(&self) -> usize {
        self.types.len() * std::mem::size_of::<HeaderType>()
            + self.source_spellings.len() * std::mem::size_of::<(HeaderTypeId, HeaderTypeId)>()
            + self.classifier_types.len() * std::mem::size_of::<HeaderClassifierType>()
            + self.type_operands.len() * std::mem::size_of::<HeaderTypeId>()
            + self.path_segments.len() * std::mem::size_of::<LookupNameId>()
            + self.parameters.len() * std::mem::size_of::<HeaderParameter>()
            + self.parameter_annotation_class_literals.len()
                * std::mem::size_of::<HeaderParameterAnnotationClassLiteral>()
            + self.type_parameters.len() * std::mem::size_of::<HeaderTypeParameter>()
            + self.bounds.len() * std::mem::size_of::<HeaderTypeBound>()
            + self.interface_delegations.len() * std::mem::size_of::<HeaderInterfaceDelegation>()
            + self.declarations.len() * std::mem::size_of::<Option<HeaderDeclaration>>()
    }
}

/// Extract syntax-independent declaration/body locations from one transient file AST. The returned
/// stubs contain no parser arena ids or owned spellings; an optional temporary lookup-name id is not
/// semantic identity. Stubs remain valid after `file` is dropped. This is the stable-identity portion
/// of pass 1; signature syntax and constraints are attached by the resolver-facing extraction pass.
pub fn extract_file_stubs(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    names: &mut LookupNames,
) -> Vec<DeclarationStub> {
    fn body_range(file: &File, body: &FunBody) -> Option<TextRange> {
        let expression = match body {
            FunBody::Expr(expression) | FunBody::Block(expression) => *expression,
            FunBody::None => return None,
        };
        file.expr_span(expression)
    }

    fn function_stub(
        _file: &File,
        source: SourceFileId,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        function: &FunDecl,
        owner: Option<DeclarationId>,
        sibling: u32,
    ) -> DeclarationStub {
        let id = ids.intern(DeclarationAnchor {
            source,
            range: function.span,
            owner,
            kind: DeclarationKind::Function,
            sibling,
        });
        let has_executable_body = !matches!(&function.body, FunBody::None)
            || function
                .params
                .iter()
                .any(|parameter| parameter.default.is_some());
        DeclarationStub {
            id,
            source,
            range: function.span,
            lookup_name: Some(names.intern(&function.name)),
            // Defaults are checked/emitted with their callable even when the declaration has no
            // ordinary body. Locate the whole declaration so a future range parser cannot omit its
            // header expressions.
            body: has_executable_body.then_some(BodyKind::Function),
            signature_inference: match (&function.ret, &function.body) {
                (None, FunBody::Expr(_)) if function.receiver.is_some() => {
                    Some(InferredSignatureKind::ExtensionExpression)
                }
                (None, FunBody::Expr(_)) => Some(InferredSignatureKind::ExpressionFunction),
                (Some(_), _) | (None, FunBody::Block(_) | FunBody::None) => None,
            },
            initialization_order: None,
            kind: DeclarationKind::Function,
            visibility: function.visibility,
            flags: DeclarationFlags::default()
                .with(DeclarationFlags::INLINE, function.is_inline())
                .with(DeclarationFlags::FINAL, function.is_final())
                .with(DeclarationFlags::OPEN, function.is_open())
                .with(DeclarationFlags::OVERRIDE, function.is_override())
                .with(DeclarationFlags::ABSTRACT, function.is_abstract())
                .with(DeclarationFlags::SUSPEND, function.is_suspend())
                .with(DeclarationFlags::TAILREC, function.is_tailrec())
                .with(DeclarationFlags::OPERATOR, function.is_operator())
                .with(DeclarationFlags::INFIX, function.is_infix())
                .with(
                    DeclarationFlags::COMPANION,
                    function.is_companion_extension(),
                ),
        }
    }

    fn generated_function_stub(
        source: SourceFileId,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        owner: DeclarationId,
        range: TextRange,
        sibling: u32,
        name: &str,
        flags: DeclarationFlags,
        visibility: Visibility,
    ) -> DeclarationStub {
        let id = ids.intern(DeclarationAnchor {
            source,
            range,
            owner: Some(owner),
            kind: DeclarationKind::Function,
            sibling,
        });
        DeclarationStub {
            id,
            source,
            range,
            lookup_name: Some(names.intern(name)),
            body: None,
            signature_inference: None,
            initialization_order: None,
            kind: DeclarationKind::Function,
            visibility,
            flags: flags.with(DeclarationFlags::COMPILER_GENERATED, true),
        }
    }

    fn property_stubs(
        file: &File,
        source: SourceFileId,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        property: &PropDecl,
        owner: Option<DeclarationId>,
        sibling: u32,
        initialization_order: Option<u32>,
        out: &mut Vec<DeclarationStub>,
    ) {
        let id = ids.intern(DeclarationAnchor {
            source,
            range: property.span,
            owner,
            kind: DeclarationKind::Property,
            sibling,
        });
        let property_body = property
            .delegate
            .map(|_| BodyKind::Delegate)
            .or_else(|| property.init.map(|_| BodyKind::Initializer));
        let inferred_expression = if property
            .explicit_backing_field
            .as_ref()
            .is_some_and(|field| field.ty.is_none())
            && property.init.is_some()
        {
            Some((
                property.init.expect("checked above"),
                BodyKind::Initializer,
                InferredSignatureKind::BackingFieldInitializer,
            ))
        } else if property.declared_ty().is_some() {
            None
        } else if let Some(delegate) = property.delegate {
            Some((
                delegate,
                BodyKind::Delegate,
                InferredSignatureKind::DelegatedProperty,
            ))
        } else if let Some(initializer) = property.init {
            Some((
                initializer,
                BodyKind::Initializer,
                if property.receiver.is_some() {
                    InferredSignatureKind::ExtensionExpression
                } else {
                    InferredSignatureKind::PropertyInitializer
                },
            ))
        } else {
            match property.getter.as_ref() {
                Some(FunBody::Expr(getter)) => Some((
                    *getter,
                    BodyKind::Getter,
                    if property.receiver.is_some() {
                        InferredSignatureKind::ExtensionExpression
                    } else {
                        InferredSignatureKind::ExpressionGetter
                    },
                )),
                Some(FunBody::Block(_) | FunBody::None) | None => None,
            }
        };
        out.push(DeclarationStub {
            id,
            source,
            range: property.span,
            lookup_name: Some(names.intern(&property.name)),
            body: property_body,
            signature_inference: inferred_expression.map(|(_, _, kind)| kind),
            initialization_order,
            kind: DeclarationKind::Property,
            visibility: property.visibility,
            flags: DeclarationFlags::default()
                .with(DeclarationFlags::EXTERNAL, property.is_external)
                .with(DeclarationFlags::EXPECT, property.is_expect)
                .with(DeclarationFlags::CONST, property.is_const)
                .with(DeclarationFlags::OPEN, property.is_open)
                .with(DeclarationFlags::OVERRIDE, property.is_override)
                .with(DeclarationFlags::ABSTRACT, property.is_abstract)
                .with(DeclarationFlags::MUTABLE, property.is_var)
                .with(DeclarationFlags::LATEINIT, property.is_lateinit)
                .with(DeclarationFlags::DELEGATED, property.delegate.is_some())
                .with(
                    DeclarationFlags::EXPLICIT_BACKING_FIELD,
                    property.explicit_backing_field.is_some(),
                )
                .with(DeclarationFlags::CUSTOM_GETTER, property.getter.is_some())
                .with(
                    DeclarationFlags::GETTER_READS_BACKING_FIELD,
                    property.getter_reads_field,
                )
                .with(DeclarationFlags::CUSTOM_SETTER, property.setter.is_some())
                .with(
                    DeclarationFlags::SETTER_HAS_BODY,
                    property
                        .setter
                        .as_ref()
                        .is_some_and(|setter| setter.body.is_some()),
                )
                .with(DeclarationFlags::HAS_INITIALIZER, property.init.is_some())
                .with(DeclarationFlags::COMPANION, property.is_companion_extension),
        });

        if property.getter_declared {
            let getter = property.getter.as_ref();
            let getter_id = ids.intern(DeclarationAnchor {
                source,
                range: getter
                    .and_then(|body| body_range(file, body))
                    .unwrap_or(property.span),
                owner: Some(id),
                kind: DeclarationKind::Accessor,
                sibling: 0,
            });
            out.push(DeclarationStub {
                id: getter_id,
                source,
                range: ids.anchor(getter_id).expect("new getter id").range,
                lookup_name: None,
                body: getter.and_then(|body| body_range(file, body).map(|_| BodyKind::Getter)),
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::Accessor,
                visibility: property.visibility,
                flags: DeclarationFlags::default()
                    .with(DeclarationFlags::INLINE, property.getter_inline),
            });
        }
        if let Some(setter) = &property.setter {
            let setter_body = setter
                .body
                .as_ref()
                .and_then(|body| body_range(file, body).map(|_| BodyKind::Setter));
            let setter_id = ids.intern(DeclarationAnchor {
                source,
                range: setter
                    .body
                    .as_ref()
                    .and_then(|body| body_range(file, body))
                    .unwrap_or(property.span),
                owner: Some(id),
                kind: DeclarationKind::Accessor,
                sibling: 1,
            });
            out.push(DeclarationStub {
                id: setter_id,
                source,
                range: ids.anchor(setter_id).expect("new setter id").range,
                lookup_name: None,
                body: setter_body,
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::Accessor,
                visibility: if setter.is_private {
                    Visibility::Private
                } else {
                    property.visibility
                },
                flags: DeclarationFlags::default().with(DeclarationFlags::INLINE, setter.is_inline),
            });
        }
    }

    fn class_stubs(
        file: &File,
        source: SourceFileId,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        class: &ClassDecl,
        is_companion: bool,
        owner: Option<DeclarationId>,
        sibling: u32,
        out: &mut Vec<DeclarationStub>,
    ) {
        let class_id = ids.intern(DeclarationAnchor {
            source,
            range: class.span,
            owner,
            kind: DeclarationKind::Classifier,
            sibling,
        });
        out.push(DeclarationStub {
            id: class_id,
            source,
            range: class.span,
            lookup_name: Some(names.intern(&class.name)),
            body: None,
            signature_inference: None,
            initialization_order: None,
            kind: DeclarationKind::Classifier,
            visibility: class.visibility,
            flags: DeclarationFlags::default()
                .with(DeclarationFlags::INTERFACE, class.is_interface())
                .with(DeclarationFlags::SINGLETON, class.is_singleton())
                .with(DeclarationFlags::DATA, class.is_data)
                .with(DeclarationFlags::VALUE, class.is_value)
                .with(DeclarationFlags::ENUM, class.is_enum())
                .with(DeclarationFlags::FUN_INTERFACE, class.is_fun_interface)
                .with(DeclarationFlags::OPEN, class.is_open())
                .with(DeclarationFlags::ABSTRACT, class.is_abstract())
                .with(DeclarationFlags::SEALED, class.is_sealed())
                .with(DeclarationFlags::FINAL, class.is_final())
                .with(DeclarationFlags::ANNOTATION_CLASS, class.is_annotation())
                .with(DeclarationFlags::INNER, class.inner_of.is_some())
                .with(DeclarationFlags::COMPANION, is_companion),
        });

        if class.primary_ctor_annotations.is_some() && !class.is_interface() {
            let constructor = ids.intern(DeclarationAnchor {
                source,
                range: class.span,
                owner: Some(class_id),
                kind: DeclarationKind::Constructor,
                sibling: 0,
            });
            out.push(DeclarationStub {
                id: constructor,
                source,
                range: class.span,
                lookup_name: None,
                body: Some(BodyKind::Constructor),
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::Constructor,
                visibility: class.primary_ctor_visibility,
                flags: DeclarationFlags::default(),
            });
        }
        if let Some(companion) = class.companion {
            let Decl::Class(companion) = file.decl(companion) else {
                panic!("a companion declaration edge must target a class")
            };
            class_stubs(
                file,
                source,
                ids,
                names,
                companion,
                true,
                Some(class_id),
                0,
                out,
            );
        }
        for (index, property) in class.props.iter().enumerate() {
            if !property.is_property {
                continue;
            }
            let id = ids.intern(DeclarationAnchor {
                source,
                range: property.span,
                owner: Some(class_id),
                kind: DeclarationKind::Property,
                sibling: u32::try_from(index).expect("too many constructor properties"),
            });
            out.push(DeclarationStub {
                id,
                source,
                range: property.span,
                lookup_name: Some(names.intern(&property.name)),
                body: None,
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::Property,
                visibility: property.visibility,
                flags: DeclarationFlags::default()
                    .with(DeclarationFlags::PROPERTY_PARAMETER, true)
                    .with(DeclarationFlags::MUTABLE, property.is_var)
                    .with(DeclarationFlags::OPEN, property.is_open)
                    .with(DeclarationFlags::OVERRIDE, property.is_override),
            });
        }
        // Data-class component/copy callables participate in ordinary overload selection during
        // Pass 1 and therefore need stable identities just like written members. They have no AST
        // declaration or body locator: their signatures come from the resolved primary-constructor
        // properties and their common-IR bodies are synthesized after function predeclaration.
        if class.is_data && !class.is_singleton() {
            let data_property_count = class
                .props
                .iter()
                .filter(|property| property.is_property)
                .count();
            for ordinal in 0..data_property_count {
                let sibling = u32::MAX
                    .checked_sub(ordinal as u32)
                    .expect("too many generated data-class components");
                out.push(generated_function_stub(
                    source,
                    ids,
                    names,
                    class_id,
                    class.span,
                    sibling,
                    &format!("component{}", ordinal + 1),
                    DeclarationFlags::default()
                        .with(DeclarationFlags::OPERATOR, true)
                        .with(DeclarationFlags::FINAL, true),
                    Visibility::Public,
                ));
            }
            out.push(generated_function_stub(
                source,
                ids,
                names,
                class_id,
                class.span,
                u32::MAX / 2,
                "copy",
                DeclarationFlags::default().with(DeclarationFlags::FINAL, true),
                if file.data_copy_respects_ctor_visibility {
                    class.primary_ctor_visibility
                } else {
                    Visibility::Public
                },
            ));
        }
        if class.is_data {
            for (ordinal, (name, parameter_count)) in
                [("toString", 0), ("hashCode", 0), ("equals", 1)]
                    .into_iter()
                    .enumerate()
            {
                if class.methods.iter().any(|method| {
                    method.receiver.is_none()
                        && method.name == name
                        && method.params.len() == parameter_count
                        && method.is_override()
                }) {
                    continue;
                }
                out.push(generated_function_stub(
                    source,
                    ids,
                    names,
                    class_id,
                    class.span,
                    u32::MAX / 2 - ordinal as u32 - 1,
                    name,
                    DeclarationFlags::default()
                        .with(DeclarationFlags::OVERRIDE, true)
                        .with(DeclarationFlags::OPERATOR, name == "equals")
                        .with(DeclarationFlags::FINAL, true),
                    Visibility::Public,
                ));
            }
        }
        for (index, constructor) in class.secondary_ctors.iter().enumerate() {
            let id = ids.intern(DeclarationAnchor {
                source,
                range: constructor.span,
                owner: Some(class_id),
                kind: DeclarationKind::Constructor,
                sibling: u32::try_from(index + 1).expect("too many secondary constructors"),
            });
            out.push(DeclarationStub {
                id,
                source,
                range: constructor.span,
                lookup_name: None,
                // A constructor is a body unit even when it has no explicit block: delegation and
                // parameter defaults are executable, and an implicit return still reaches lowering.
                body: Some(BodyKind::Constructor),
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::Constructor,
                visibility: Visibility::Public,
                flags: DeclarationFlags::default(),
            });
        }
        for (index, method) in class.methods.iter().enumerate() {
            out.push(function_stub(
                file,
                source,
                ids,
                names,
                method,
                Some(class_id),
                u32::try_from(index).expect("too many class methods"),
            ));
        }
        for (index, property) in class.body_props.iter().enumerate() {
            property_stubs(
                file,
                source,
                ids,
                names,
                property,
                Some(class_id),
                u32::try_from(index).expect("too many class properties"),
                class
                    .init_order
                    .iter()
                    .position(
                        |step| matches!(step, ClassInit::PropInit(property) if *property == index),
                    )
                    .map(|order| u32::try_from(order).expect("too many class initializers")),
                out,
            );
        }
        for (index, initializer) in class.init_order.iter().enumerate() {
            let ClassInit::Block(expression) = initializer else {
                continue;
            };
            // Active Pass-1 default binding may revisit the compact declaration stream after an
            // unrelated ordinary init body has been released. Its owner/kind/sibling structure is
            // still exact; use the owning declaration range only as non-semantic diagnostic data.
            let range = file.expr_span(*expression).unwrap_or(class.span);
            let id = ids.intern(DeclarationAnchor {
                source,
                range,
                owner: Some(class_id),
                kind: DeclarationKind::Initializer,
                sibling: u32::try_from(index).expect("too many class initializers"),
            });
            out.push(DeclarationStub {
                id,
                source,
                range,
                lookup_name: None,
                body: Some(BodyKind::Initializer),
                signature_inference: None,
                initialization_order: Some(
                    u32::try_from(index).expect("too many class initializers"),
                ),
                kind: DeclarationKind::Initializer,
                visibility: Visibility::Private,
                flags: DeclarationFlags::default(),
            });
        }
        for (index, alias) in class.type_aliases.iter().enumerate() {
            let id = ids.intern(DeclarationAnchor {
                source,
                range: alias.span,
                owner: Some(class_id),
                kind: DeclarationKind::TypeAlias,
                sibling: u32::try_from(index).expect("too many nested type aliases"),
            });
            out.push(DeclarationStub {
                id,
                source,
                range: alias.span,
                lookup_name: Some(names.intern(&alias.name)),
                body: None,
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::TypeAlias,
                visibility: Visibility::Public,
                flags: DeclarationFlags::default(),
            });
        }
        for (entry_index, entry) in class.enum_entries.iter().enumerate() {
            let entry_id = ids.intern(DeclarationAnchor {
                source,
                range: entry.span,
                owner: Some(class_id),
                kind: DeclarationKind::EnumEntry,
                sibling: u32::try_from(entry_index).expect("too many enum entries"),
            });
            out.push(DeclarationStub {
                id: entry_id,
                source,
                range: entry.span,
                lookup_name: Some(names.intern(&entry.name)),
                // Every enum entry is constructed, including the zero-source-argument form. The
                // backend supplies only physical name/ordinal parameters; the semantic primary
                // constructor selection still belongs to checked FIR.
                body: Some(BodyKind::EnumEntry),
                signature_inference: None,
                initialization_order: None,
                kind: DeclarationKind::EnumEntry,
                visibility: Visibility::Public,
                flags: DeclarationFlags::default(),
            });
            for (method_index, method) in entry.methods.iter().enumerate() {
                out.push(function_stub(
                    file,
                    source,
                    ids,
                    names,
                    method,
                    Some(entry_id),
                    u32::try_from(method_index).expect("too many enum-entry methods"),
                ));
            }
            for (property_index, property) in entry.props.iter().enumerate() {
                property_stubs(
                    file,
                    source,
                    ids,
                    names,
                    property,
                    Some(entry_id),
                    u32::try_from(property_index).expect("too many enum-entry properties"),
                    entry
                        .init_order
                        .iter()
                        .position(|step| {
                            matches!(step, ClassInit::PropInit(property) if *property == property_index)
                        })
                        .map(|order| {
                            u32::try_from(order).expect("too many enum-entry initializers")
                        }),
                    out,
                );
            }
            for (initializer_index, initializer) in entry.init_order.iter().enumerate() {
                let ClassInit::Block(expression) = initializer else {
                    continue;
                };
                let range = file
                    .expr_span(*expression)
                    .expect("an enum-entry init body must have a source span");
                let id = ids.intern(DeclarationAnchor {
                    source,
                    range,
                    owner: Some(entry_id),
                    kind: DeclarationKind::Initializer,
                    sibling: u32::try_from(initializer_index)
                        .expect("too many enum-entry initializers"),
                });
                out.push(DeclarationStub {
                    id,
                    source,
                    range,
                    lookup_name: None,
                    body: Some(BodyKind::Initializer),
                    signature_inference: None,
                    initialization_order: Some(
                        u32::try_from(initializer_index).expect("too many enum-entry initializers"),
                    ),
                    kind: DeclarationKind::Initializer,
                    visibility: Visibility::Private,
                    flags: DeclarationFlags::default(),
                });
            }
        }
    }

    // Nested classifiers of a parser-hoisted local class are separate `file.decls` entries. Give
    // them the stable classifier owner they have semantically before emitting any stubs; otherwise
    // their apparent file-level position leaks through as module ownership and an inner class loses
    // the enclosing-instance identity needed by checked FIR.
    let nested_owners = nested_classifier_owners(file, source, ids);
    let companion_declarations = companion_declarations(file);
    let mut stubs = Vec::new();
    let mut declaration_blocks = Vec::new();
    for (index, declaration) in file.decls.iter().enumerate() {
        if companion_declarations.contains(declaration) {
            continue;
        }
        let first_stub = stubs.len();
        match file.decl(*declaration) {
            Decl::Fun(function) => stubs.push(function_stub(
                file,
                source,
                ids,
                names,
                function,
                None,
                u32::try_from(index).expect("too many file declarations"),
            )),
            Decl::Property(property) => property_stubs(
                file,
                source,
                ids,
                names,
                property,
                None,
                u32::try_from(index).expect("too many file declarations"),
                None,
                &mut stubs,
            ),
            Decl::Class(class) => class_stubs(
                file,
                source,
                ids,
                names,
                class,
                false,
                nested_owners.get(declaration).copied(),
                u32::try_from(index).expect("too many file declarations"),
                &mut stubs,
            ),
        }
        if file.is_local_declaration(*declaration) {
            for stub in &mut stubs[first_stub..] {
                stub.flags = stub.flags.with(DeclarationFlags::LOCAL_CLASS, true);
            }
        }
        if file
            .anonymous_object_classes
            .values()
            .any(|anonymous| anonymous == declaration)
        {
            for stub in &mut stubs[first_stub..] {
                stub.flags = stub.flags.with(DeclarationFlags::LOCAL_CLASS, true);
            }
            stubs[first_stub].flags = stubs[first_stub]
                .flags
                .with(DeclarationFlags::ANONYMOUS_OBJECT, true);
        }
        if file.expect_decls.contains(declaration) {
            stubs[first_stub].flags = stubs[first_stub].flags.with(DeclarationFlags::EXPECT, true);
        }
        declaration_blocks.push((*declaration, first_stub..stubs.len()));
    }
    if !file.local_class_enclosing_declarations.is_empty() {
        let blocks = declaration_blocks
            .iter()
            .cloned()
            .collect::<std::collections::HashMap<_, _>>();
        let mut declaration_order = Vec::with_capacity(declaration_blocks.len());
        let mut added = std::collections::HashSet::new();
        for declaration in file.decls.iter().copied() {
            if companion_declarations.contains(&declaration)
                || file
                    .local_class_enclosing_declarations
                    .contains_key(&declaration)
            {
                continue;
            }
            if blocks.contains_key(&declaration) && added.insert(declaration) {
                declaration_order.push(declaration);
            }
            for local in file.decls.iter().copied().filter(|local| {
                file.local_class_enclosing_declarations.get(local) == Some(&declaration)
            }) {
                if blocks.contains_key(&local) && added.insert(local) {
                    declaration_order.push(local);
                }
            }
        }
        // Anonymous classifiers and recovered declarations can have another transient ownership
        // representation. Preserve their parser order; the local-class relation above only moves
        // declarations for which the parser supplied an explicit lexical edge.
        for (declaration, _) in &declaration_blocks {
            if added.insert(*declaration) {
                declaration_order.push(*declaration);
            }
        }
        let mut ordered = Vec::with_capacity(stubs.len());
        for declaration in declaration_order {
            if let Some(range) = blocks.get(&declaration) {
                ordered.extend_from_slice(&stubs[range.clone()]);
            }
        }
        debug_assert_eq!(ordered.len(), stubs.len());
        stubs = ordered;
    }
    for (index, alias) in file.type_alias_decls.iter().enumerate() {
        let id = ids.intern(DeclarationAnchor {
            source,
            range: alias.span,
            owner: None,
            kind: DeclarationKind::TypeAlias,
            sibling: u32::try_from(index).expect("too many file type aliases"),
        });
        stubs.push(DeclarationStub {
            id,
            source,
            range: alias.span,
            lookup_name: Some(names.intern(&alias.name)),
            body: None,
            signature_inference: None,
            initialization_order: None,
            kind: DeclarationKind::TypeAlias,
            visibility: file
                .type_alias_visibility
                .get(&alias.name)
                .copied()
                .unwrap_or(Visibility::Public),
            flags: DeclarationFlags::default(),
        });
    }
    if let Some(script) = file.script_body {
        let range = file
            .expr_span(script)
            .expect("a script body must have a parser-owned source span");
        let id = ids.intern(DeclarationAnchor {
            source,
            range,
            owner: None,
            kind: DeclarationKind::Script,
            sibling: 0,
        });
        stubs.push(DeclarationStub {
            id,
            source,
            range,
            lookup_name: None,
            body: Some(BodyKind::Script),
            signature_inference: None,
            initialization_order: None,
            kind: DeclarationKind::Script,
            visibility: Visibility::Private,
            flags: DeclarationFlags::default(),
        });
    }
    let mut body_local = stubs
        .iter()
        .filter(|stub| stub.flags.has(DeclarationFlags::LOCAL_CLASS))
        .map(|stub| stub.id)
        .collect::<std::collections::HashSet<_>>();
    loop {
        let mut changed = false;
        for stub in &mut stubs {
            let inherited = ids
                .anchor(stub.id)
                .and_then(|anchor| anchor.owner)
                .is_some_and(|owner| body_local.contains(&owner));
            if inherited && body_local.insert(stub.id) {
                stub.flags = stub.flags.with(DeclarationFlags::LOCAL_CLASS, true);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    stubs
}

/// Copy every non-local declaration type from one transient file into packed header storage. This
/// deliberately does not walk bodies: local declarations and body type uses are checked when their
/// body unit is reparsed in Pass 2.
pub fn extract_file_header_syntax(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    names: &mut LookupNames,
    headers: &mut HeaderSyntaxArena,
) {
    fn id_for(
        ids: &mut DeclarationIds,
        source: SourceFileId,
        range: Span,
        owner: Option<DeclarationId>,
        kind: DeclarationKind,
        sibling: u32,
    ) -> DeclarationId {
        ids.intern(DeclarationAnchor {
            source,
            range,
            owner,
            kind,
            sibling,
        })
    }

    fn parameters(
        file: &File,
        headers: &mut HeaderSyntaxArena,
        names: &mut LookupNames,
        values: &[Param],
        type_annotations: &std::collections::HashMap<u32, Vec<crate::ast::AnnotationRef>>,
    ) -> HeaderParameterRange {
        fn qualifier_segments(
            file: &File,
            expression: crate::ast::ExprId,
            out: &mut Vec<String>,
        ) -> bool {
            match file.expr(expression) {
                Expr::Name(name) => {
                    out.push(name.clone());
                    true
                }
                Expr::Member { receiver, name } => {
                    if !qualifier_segments(file, *receiver, out) {
                        return false;
                    }
                    out.push(name.clone());
                    true
                }
                _ => false,
            }
        }

        let mut packed = Vec::with_capacity(values.len());
        for parameter in values {
            let annotations = annotation_types(headers, names, &parameter.annotations);
            let type_annotations = annotation_types(
                headers,
                names,
                type_annotations
                    .get(&parameter.ty.span.lo)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            );
            let mut annotation_class_literals = Vec::new();
            for (annotation_ordinal, arguments) in parameter.annotation_args.iter().enumerate() {
                let Some(&argument) = arguments.first() else {
                    continue;
                };
                let Expr::CallableRef {
                    receiver: Some(receiver),
                    name,
                } = file.expr(argument)
                else {
                    continue;
                };
                if name != "class" {
                    continue;
                }
                let mut segments = Vec::new();
                if !qualifier_segments(file, *receiver, &mut segments) || segments.is_empty() {
                    continue;
                }
                let classifier = headers.add_path(&segments.join("."), names);
                annotation_class_literals.push(HeaderParameterAnnotationClassLiteral {
                    annotation_ordinal: u32::try_from(annotation_ordinal)
                        .expect("too many parameter annotations"),
                    classifier,
                });
            }
            let annotation_class_literals =
                headers.add_parameter_annotation_class_literal_range(annotation_class_literals);
            packed.push(HeaderParameter {
                name: names.intern(&parameter.name),
                ty: headers.add_type(&parameter.ty, names),
                flags: HeaderParameterFlags::default()
                    .with(HeaderParameterFlags::VARARG, parameter.is_vararg)
                    .with(HeaderParameterFlags::DEFAULT, parameter.default.is_some()),
                span: parameter.ty.span,
                annotations,
                type_annotations,
                annotation_class_literals,
            });
        }
        headers.add_parameter_range(packed)
    }

    fn primary_parameters(
        headers: &mut HeaderSyntaxArena,
        names: &mut LookupNames,
        values: &[PropParam],
        type_annotations: &std::collections::HashMap<u32, Vec<crate::ast::AnnotationRef>>,
    ) -> HeaderParameterRange {
        let mut packed = Vec::with_capacity(values.len());
        for parameter in values {
            let annotations = annotation_types(headers, names, &parameter.annotations);
            let type_annotations = annotation_types(
                headers,
                names,
                type_annotations
                    .get(&parameter.ty.span.lo)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            );
            packed.push(HeaderParameter {
                name: names.intern(&parameter.name),
                ty: headers.add_type(&parameter.ty, names),
                flags: HeaderParameterFlags::default()
                    .with(HeaderParameterFlags::VARARG, parameter.is_vararg)
                    .with(HeaderParameterFlags::DEFAULT, parameter.default.is_some())
                    .with(HeaderParameterFlags::PROPERTY, parameter.is_property)
                    .with(
                        HeaderParameterFlags::MUTABLE_PROPERTY,
                        parameter.is_property && parameter.is_var,
                    ),
                span: if parameter.span == Span::new(0, 0) {
                    parameter.ty.span
                } else {
                    parameter.span
                },
                annotations,
                type_annotations,
                annotation_class_literals: HeaderParameterAnnotationClassLiteralRange::default(),
            });
        }
        headers.add_parameter_range(packed)
    }

    fn annotation_types(
        headers: &mut HeaderSyntaxArena,
        names: &mut LookupNames,
        annotations: &[crate::ast::AnnotationRef],
    ) -> HeaderTypeRange {
        let annotations = annotations.iter().map(TypeRef::from_annotation);
        let types = annotations
            .map(|annotation| headers.add_type(&annotation, names))
            .collect::<Vec<_>>();
        headers.add_type_range(types)
    }

    fn type_parameters(
        headers: &mut HeaderSyntaxArena,
        names: &mut LookupNames,
        values: &[String],
        variances: &[crate::types::TypeVariance],
        non_null: impl Fn(&str) -> bool,
        reified: impl Fn(&str) -> bool,
    ) -> HeaderTypeParameterRange {
        let packed = values
            .iter()
            .enumerate()
            .map(|(index, name)| HeaderTypeParameter {
                name: names.intern(name),
                flags: HeaderTypeParameterFlags::new(
                    variances
                        .get(index)
                        .copied()
                        .unwrap_or(crate::types::TypeVariance::Invariant),
                    non_null(name),
                    reified(name),
                ),
            });
        headers.add_type_parameter_range(packed.collect::<Vec<_>>())
    }

    fn bounds(
        headers: &mut HeaderSyntaxArena,
        names: &mut LookupNames,
        values: &[(String, TypeRef)],
    ) -> HeaderTypeBoundRange {
        let mut packed = Vec::with_capacity(values.len());
        for (parameter, ty) in values {
            packed.push(HeaderTypeBound {
                parameter: names.intern(parameter),
                ty: headers.add_type(ty, names),
            });
        }
        headers.add_bound_range(packed)
    }

    fn function(
        file: &File,
        source: SourceFileId,
        owner: Option<DeclarationId>,
        sibling: u32,
        function: &FunDecl,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        headers: &mut HeaderSyntaxArena,
    ) {
        let declaration = id_for(
            ids,
            source,
            function.span,
            owner,
            DeclarationKind::Function,
            sibling,
        );
        let receiver = function
            .receiver
            .as_ref()
            .map(|receiver| headers.add_type(receiver, names));
        let parameters = parameters(
            file,
            headers,
            names,
            &function.params,
            &file.type_annotations,
        );
        let annotations = annotation_types(headers, names, &function.annotations);
        let result = match (&function.ret, &function.body) {
            (Some(result), _) => HeaderResultType::Explicit(headers.add_type(result, names)),
            (None, FunBody::Block(_) | FunBody::None) => HeaderResultType::ImplicitUnit,
            (None, FunBody::Expr(_)) => HeaderResultType::Inferred,
        };
        let type_parameters = type_parameters(
            headers,
            names,
            &function.type_params,
            &[],
            |name| function.non_null_type_params.contains(name),
            |name| function.reified_type_params.contains(name),
        );
        let bounds = bounds(headers, names, &function.type_param_bounds);
        headers.insert_declaration(HeaderDeclaration {
            declaration,
            annotations,
            kind: HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                result,
                type_parameters,
                bounds,
                context_count: u32::try_from(function.context_count)
                    .expect("too many callable context parameters"),
                signature_start: function.signature_span.lo,
                signature_end: function.signature_span.hi,
            },
        });
    }

    fn property(
        file: &File,
        source: SourceFileId,
        owner: Option<DeclarationId>,
        sibling: u32,
        property: &PropDecl,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        headers: &mut HeaderSyntaxArena,
    ) {
        let declaration = id_for(
            ids,
            source,
            property.span,
            owner,
            DeclarationKind::Property,
            sibling,
        );
        let receiver = property
            .receiver
            .as_ref()
            .map(|receiver| headers.add_type(receiver, names));
        let context_parameters = parameters(
            file,
            headers,
            names,
            &property.context_params,
            &file.type_annotations,
        );
        let annotations = annotation_types(headers, names, &property.annotations);
        let declared_type = property.ty.as_ref().map(|ty| headers.add_type(ty, names));
        let getter_type = property
            .getter_ty
            .as_ref()
            .map(|ty| headers.add_type(ty, names));
        let backing_field_type = property
            .explicit_backing_field
            .as_ref()
            .and_then(|field| field.ty.as_ref())
            .map(|ty| headers.add_type(ty, names));
        let type_parameters = type_parameters(
            headers,
            names,
            &property.type_params,
            &[],
            |_| false,
            |_| false,
        );
        let bounds = bounds(headers, names, &property.type_param_bounds);
        headers.insert_declaration(HeaderDeclaration {
            declaration,
            annotations,
            kind: HeaderDeclarationKind::Property {
                receiver,
                context_parameters,
                declared_type,
                getter_type,
                backing_field_type,
                type_parameters,
                bounds,
                mutable: property.is_var,
            },
        });
    }

    fn alias(
        source: SourceFileId,
        owner: Option<DeclarationId>,
        sibling: u32,
        alias: &TypeAliasDecl,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        headers: &mut HeaderSyntaxArena,
    ) {
        let declaration = id_for(
            ids,
            source,
            alias.span,
            owner,
            DeclarationKind::TypeAlias,
            sibling,
        );
        let type_parameters = type_parameters(
            headers,
            names,
            &alias.type_params,
            &[],
            |_| false,
            |_| false,
        );
        let target = headers.add_type(&alias.target, names);
        headers.insert_declaration(HeaderDeclaration {
            declaration,
            annotations: HeaderTypeRange::default(),
            kind: HeaderDeclarationKind::TypeAlias {
                type_parameters,
                target,
            },
        });
    }

    fn classifier(
        file: &File,
        source: SourceFileId,
        owner: Option<DeclarationId>,
        sibling: u32,
        class: &ClassDecl,
        ids: &mut DeclarationIds,
        names: &mut LookupNames,
        headers: &mut HeaderSyntaxArena,
    ) {
        let declaration = id_for(
            ids,
            source,
            class.span,
            owner,
            DeclarationKind::Classifier,
            sibling,
        );
        let declared_type_parameters = type_parameters(
            headers,
            names,
            class.type_params(),
            class.type_param_variances(),
            |_| false,
            |_| false,
        );
        let lexical_type_parameter_captures = type_parameters(
            headers,
            names,
            &class.lexical_type_parameter_captures,
            &[],
            |_| false,
            |_| false,
        );
        let bounds = bounds(headers, names, class.type_param_bounds());
        let supertype_ids = class
            .supertypes
            .iter()
            .map(|ty| headers.add_type(ty, names))
            .collect::<Vec<_>>();
        let base = class.base_class.as_deref().map(|base| {
            headers.add_classifier_type(
                base,
                &class.base_type_args,
                class.base_class_span.unwrap_or(class.span),
                names,
            )
        });
        let supertypes = headers.add_type_range(supertype_ids);
        let context_parameters = parameters(
            file,
            headers,
            names,
            &class.context_params,
            &file.type_annotations,
        );
        let primary_parameters =
            primary_parameters(headers, names, &class.props, &file.type_annotations);
        let annotations = annotation_types(headers, names, &class.annotations);
        let delegations = headers.add_interface_delegation_range(
            class.interface_delegations.iter().filter_map(|delegation| {
                let supertype = delegation.supertype?;
                class.supertypes.get(supertype as usize)?;
                let source = delegation
                    .bare_name
                    .as_ref()
                    .and_then(|parameter_name| {
                        class
                            .props
                            .iter()
                            .position(|parameter| parameter.name == *parameter_name)
                    })
                    .and_then(|parameter| u32::try_from(parameter).ok())
                    .map(HeaderInterfaceDelegateSource::ConstructorParameter)
                    .unwrap_or(HeaderInterfaceDelegateSource::ConstructorBodyInitializer);
                Some(HeaderInterfaceDelegation { supertype, source })
            }),
        );
        headers.insert_declaration(HeaderDeclaration {
            declaration,
            annotations,
            kind: HeaderDeclarationKind::Classifier {
                type_parameters: declared_type_parameters,
                lexical_type_parameter_captures,
                bounds,
                supertypes,
                base,
                context_parameters,
                primary_parameters,
                delegations,
            },
        });

        if class.primary_ctor_annotations.is_some() && !class.is_interface() {
            let constructor = id_for(
                ids,
                source,
                class.span,
                Some(declaration),
                DeclarationKind::Constructor,
                0,
            );
            let annotations = annotation_types(
                headers,
                names,
                class
                    .primary_ctor_annotations
                    .as_deref()
                    .unwrap_or_default(),
            );
            headers.insert_declaration(HeaderDeclaration {
                declaration: constructor,
                annotations,
                kind: HeaderDeclarationKind::Constructor {
                    context_parameters,
                    parameters: primary_parameters,
                },
            });
        }
        if let Some(companion) = class.companion {
            let Decl::Class(companion) = file.decl(companion) else {
                panic!("a companion declaration edge must target a class")
            };
            classifier(
                file,
                source,
                Some(declaration),
                0,
                companion,
                ids,
                names,
                headers,
            );
        }
        for (index, parameter) in class.props.iter().enumerate() {
            if !parameter.is_property {
                continue;
            }
            let property_id = id_for(
                ids,
                source,
                parameter.span,
                Some(declaration),
                DeclarationKind::Property,
                u32::try_from(index).expect("too many constructor properties"),
            );
            let declared_type = Some(headers.add_type(&parameter.ty, names));
            let annotations = annotation_types(headers, names, &parameter.annotations);
            headers.insert_declaration(HeaderDeclaration {
                declaration: property_id,
                annotations,
                kind: HeaderDeclarationKind::Property {
                    receiver: None,
                    context_parameters: HeaderParameterRange::default(),
                    declared_type,
                    getter_type: None,
                    backing_field_type: None,
                    type_parameters: HeaderTypeParameterRange::default(),
                    bounds: HeaderTypeBoundRange::default(),
                    mutable: parameter.is_var,
                },
            });
        }
        for (index, constructor) in class.secondary_ctors.iter().enumerate() {
            let constructor_id = id_for(
                ids,
                source,
                constructor.span,
                Some(declaration),
                DeclarationKind::Constructor,
                u32::try_from(index + 1).expect("too many secondary constructors"),
            );
            let parameters = parameters(
                file,
                headers,
                names,
                &constructor.params,
                &file.type_annotations,
            );
            let annotations = annotation_types(headers, names, &constructor.annotations);
            headers.insert_declaration(HeaderDeclaration {
                declaration: constructor_id,
                annotations,
                kind: HeaderDeclarationKind::Constructor {
                    context_parameters,
                    parameters,
                },
            });
        }
        for (index, method) in class.methods.iter().enumerate() {
            function(
                file,
                source,
                Some(declaration),
                u32::try_from(index).expect("too many class methods"),
                method,
                ids,
                names,
                headers,
            );
        }
        for (index, value) in class.body_props.iter().enumerate() {
            property(
                file,
                source,
                Some(declaration),
                u32::try_from(index).expect("too many class properties"),
                value,
                ids,
                names,
                headers,
            );
        }
        for (index, value) in class.type_aliases.iter().enumerate() {
            alias(
                source,
                Some(declaration),
                u32::try_from(index).expect("too many nested type aliases"),
                value,
                ids,
                names,
                headers,
            );
        }
        for (entry_index, entry) in class.enum_entries.iter().enumerate() {
            let entry_id = id_for(
                ids,
                source,
                entry.span,
                Some(declaration),
                DeclarationKind::EnumEntry,
                u32::try_from(entry_index).expect("too many enum entries"),
            );
            for (index, method) in entry.methods.iter().enumerate() {
                function(
                    file,
                    source,
                    Some(entry_id),
                    u32::try_from(index).expect("too many enum-entry methods"),
                    method,
                    ids,
                    names,
                    headers,
                );
            }
            for (index, value) in entry.props.iter().enumerate() {
                property(
                    file,
                    source,
                    Some(entry_id),
                    u32::try_from(index).expect("too many enum-entry properties"),
                    value,
                    ids,
                    names,
                    headers,
                );
            }
        }
    }

    let nested_owners = nested_classifier_owners(file, source, ids);
    let companion_declarations = companion_declarations(file);
    for (index, declaration) in file.decls.iter().enumerate() {
        if companion_declarations.contains(declaration) {
            continue;
        }
        match file.decl(*declaration) {
            Decl::Fun(value) => function(
                file,
                source,
                None,
                u32::try_from(index).expect("too many file declarations"),
                value,
                ids,
                names,
                headers,
            ),
            Decl::Property(value) => property(
                file,
                source,
                None,
                u32::try_from(index).expect("too many file declarations"),
                value,
                ids,
                names,
                headers,
            ),
            Decl::Class(value) => classifier(
                file,
                source,
                nested_owners.get(declaration).copied(),
                u32::try_from(index).expect("too many file declarations"),
                value,
                ids,
                names,
                headers,
            ),
        }
    }
    for (index, value) in file.type_alias_decls.iter().enumerate() {
        alias(
            source,
            None,
            u32::try_from(index).expect("too many file type aliases"),
            value,
            ids,
            names,
            headers,
        );
    }
}

/// AST-free result of the streamed declaration-inventory pass. Signature extraction consumes this
/// together with the callback's compact header/constraint output; it never stores a parsed file.
pub struct StreamedHeaderModule {
    pub sources: SourceMap,
    /// Diagnostic origins owned by the temporary signature graph. They are deliberately separate
    /// from `sources.origins`: consuming this header module destroys every signature-expression
    /// coordinate, while the source map retains only origins referenced by checked inline/default
    /// FIR.
    pub(crate) signature_origins: super::body::OriginStore,
    pub declarations: DeclarationIds,
    pub lookup_names: LookupNames,
    pub scopes: HeaderScopeArena,
    pub syntax: HeaderSyntaxArena,
    /// Detached declaration annotation/type occurrences needed by Pass-1 import and annotation
    /// resolution. Their packed type nodes live in `syntax`; no parser `TypeRef` or arena identity
    /// survives the active source parse.
    pub(super) detached_types: Vec<(SourceFileId, HeaderTypeId)>,
    annotation_strings: HeaderAnnotationStringArena,
    annotation_policies: HeaderAnnotationPolicyArena,
    visibility_suppressions: HeaderVisibilitySuppressionArena,
    pub stubs: Vec<DeclarationStub>,
    /// Complete parser declaration-stream order before semantic exclusions. These are stable
    /// header identities, not source offsets or parser arena ids.
    pub(super) inventory: Vec<DeclarationId>,
    /// Pass-1-only semantic containment for parser-hoisted local classifiers. Both sides are stable
    /// declaration identities derived while the AST is live; no source coordinate or parser arena
    /// identity is retained. Inline/default preparation consumes this before Pass 2.
    pub(super) local_classifier_lexical_roots:
        std::collections::HashMap<DeclarationId, DeclarationId>,
    /// Whether each source contributed compact declaration headers. A Java input, or a Kotlin input
    /// whose parse failed, has a stable file identity but no header inventory: its declarations were
    /// never walked, so a header lookup against it must not be read as a missing-header defect.
    pub(super) inventoried: Vec<bool>,
    /// Stable declarations intentionally removed by expect/actual actualization. The transient
    /// legacy signature bridge may still be walking the reparsed source file, so it must distinguish
    /// an excluded header from an inventory defect.
    pub(super) excluded: std::collections::HashSet<DeclarationId>,
}

impl StreamedHeaderModule {
    /// Stable declaration-stream order captured while the bounded source unit is active. Signature
    /// solving may compare declaration order through these identities; it must never recover that
    /// order from text offsets or parser arena coordinates.
    pub(crate) fn declaration_inventory(&self) -> &[DeclarationId] {
        &self.inventory
    }

    /// Whether `source` contributed compact declaration headers. Semantic consumers that iterate a
    /// whole parsed source set must consult this before requiring a compact header: an unparsable
    /// file still owns a `SourceFileId` and still has recovered AST declarations.
    pub fn has_headers(&self, source: SourceFileId) -> bool {
        self.inventoried
            .get(source.raw() as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Qualified, JVM-neutral identities of non-local Kotlin classifiers published by this
    /// source module. Platform source providers use this once during Pass 1 to resolve foreign
    /// declaration headers against Kotlin declarations from the same compilation. Local and
    /// anonymous classifiers are deliberately absent: another source file cannot name them.
    pub fn source_classifier_names(&self) -> Vec<crate::types::TypeName> {
        let mut names = self
            .stubs
            .iter()
            .filter(|stub| stub.kind == DeclarationKind::Classifier)
            .filter(|stub| {
                !stub.flags.has(DeclarationFlags::LOCAL_CLASS)
                    && !stub.flags.has(DeclarationFlags::ANONYMOUS_OBJECT)
            })
            .filter_map(|stub| {
                let spelling = self.lookup_names.get(stub.lookup_name?)?;
                let mut segments = spelling
                    .split(['.', '$'])
                    .filter(|segment| !segment.is_empty());
                let first = segments.next()?;
                let package = self.sources.get(stub.source)?.package;
                let mut identity = crate::types::type_name_child(package, first);
                for segment in segments {
                    identity = crate::types::type_name_nested_child(identity, segment);
                }
                Some(identity)
            })
            .collect::<Vec<_>>();
        let mut seen = std::collections::HashSet::new();
        names.retain(|name| seen.insert(*name));
        names
    }

    pub(crate) fn local_classifier_lexical_root(
        &self,
        declaration: DeclarationId,
    ) -> Option<DeclarationId> {
        self.local_classifier_lexical_roots
            .get(&declaration)
            .copied()
    }

    /// Remove declaration subtrees selected out of the module before signature extraction. Stable
    /// anchors remain interned, but no excluded header can contribute lookup candidates,
    /// constraints, or ordinary body-unit work. This is used by expect/actual actualization after compact
    /// matching has selected an actual declaration and before ordinary signature resolution runs.
    pub fn exclude_declaration_subtrees(
        &mut self,
        roots: &std::collections::HashSet<DeclarationId>,
    ) {
        fn is_excluded(
            declarations: &DeclarationIds,
            roots: &std::collections::HashSet<DeclarationId>,
            declaration: DeclarationId,
        ) -> bool {
            let mut current = Some(declaration);
            let mut remaining = declarations.len().saturating_add(1);
            while let Some(candidate) = current {
                if roots.contains(&candidate) {
                    return true;
                }
                if remaining == 0 {
                    panic!("stable declaration ownership graph contains a cycle");
                }
                remaining -= 1;
                current = declarations
                    .anchor(candidate)
                    .and_then(|anchor| anchor.owner);
            }
            false
        }

        let excluded = self
            .stubs
            .iter()
            .filter(|stub| is_excluded(&self.declarations, roots, stub.id))
            .map(|stub| stub.id)
            .collect::<Vec<_>>();
        self.excluded.extend(excluded);
        self.stubs.retain(|stub| !self.excluded.contains(&stub.id));
    }

    /// Persistent payload controlled by this header inventory. Source text and transient AST arenas
    /// are absent, so ordinary body growth cannot affect the result.
    pub fn storage_payload_bytes(&self) -> usize {
        self.sources.path_payload_bytes()
            + self.signature_origins.storage_payload_bytes()
            + self.declarations.storage_payload_bytes()
            + self.lookup_names.storage_payload_bytes()
            + self.scopes.storage_payload_bytes()
            + self.syntax.storage_payload_bytes()
            + self.detached_types.len() * std::mem::size_of::<(SourceFileId, HeaderTypeId)>()
            + self.annotation_strings.storage_payload_bytes()
            + self.annotation_policies.storage_payload_bytes()
            + self.visibility_suppressions.storage_payload_bytes()
            + self.stubs.len() * std::mem::size_of::<DeclarationStub>()
            + self.inventory.len() * std::mem::size_of::<DeclarationId>()
            + self.excluded.len() * std::mem::size_of::<DeclarationId>()
    }

    pub(crate) fn annotation_policy_applications(
        &self,
        declaration: DeclarationId,
    ) -> &[HeaderAnnotationPolicyApplication] {
        self.annotation_policies.applications(declaration)
    }

    /// Constant string arguments attached to one declaration annotation. Pass 1 copies the values
    /// while the bounded expression arena is live; signature consumers resolve the annotation at
    /// the same declaration-local ordinal before interpreting them. No parser `ExprId` or source
    /// coordinate crosses the pass boundary.
    pub(crate) fn annotation_string_arguments(
        &self,
        declaration: DeclarationId,
        annotation_ordinal: usize,
    ) -> &[Box<str>] {
        self.annotation_strings
            .arguments(declaration, annotation_ordinal)
    }

    pub(crate) fn annotation_policy_arguments(
        &self,
        range: HeaderAnnotationArgumentRange,
    ) -> &[LookupNameId] {
        self.annotation_policies.arguments(range)
    }

    pub(crate) fn file_visibility_suppressions(
        &self,
        source: SourceFileId,
    ) -> &[HeaderVisibilitySuppressionApplication] {
        self.visibility_suppressions.file(source)
    }

    pub(crate) fn declaration_visibility_suppressions(
        &self,
        declaration: DeclarationId,
    ) -> &[HeaderVisibilitySuppressionApplication] {
        self.visibility_suppressions.declaration(declaration)
    }

    pub(crate) fn detached_type_roots(
        &self,
        source: SourceFileId,
    ) -> impl Iterator<Item = HeaderTypeId> + '_ {
        self.detached_types
            .iter()
            .filter(move |(candidate, _)| *candidate == source)
            .map(|(_, ty)| *ty)
    }
}

/// Provider-neutral constant payload for declaration annotations needed during signature solving.
/// The arena is deliberately generic: `@JvmName`, plugin annotations, and future signature-affecting
/// annotations must not each invent a private copy of Pass-1 expression extraction.
#[derive(Default)]
struct HeaderAnnotationStringArena {
    /// Entries are parallel to the declaration's annotation list. Empty argument slices preserve
    /// ordinals without retaining annotation spellings or source ranges.
    declarations: std::collections::HashMap<DeclarationId, Vec<Box<[Box<str>]>>>,
}

impl HeaderAnnotationStringArena {
    fn add(
        &mut self,
        declaration: DeclarationId,
        file: &File,
        annotations: &[crate::ast::AnnotationRef],
        source_arguments: &[Vec<crate::ast::ExprId>],
    ) {
        let arguments = annotations
            .iter()
            .zip(source_arguments)
            .map(|(_, arguments)| {
                arguments
                    .iter()
                    .filter_map(|argument| file.const_string_value(*argument))
                    .filter_map(|value| value.as_str().map(|value| value.into()))
                    .collect::<Vec<Box<str>>>()
                    .into_boxed_slice()
            })
            .collect::<Vec<_>>();
        if arguments.iter().any(|arguments| !arguments.is_empty()) {
            self.declarations.insert(declaration, arguments);
        }
    }

    fn arguments(&self, declaration: DeclarationId, annotation_ordinal: usize) -> &[Box<str>] {
        self.declarations
            .get(&declaration)
            .and_then(|annotations| annotations.get(annotation_ordinal))
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    fn storage_payload_bytes(&self) -> usize {
        self.declarations
            .values()
            .map(|annotations| {
                annotations.len() * std::mem::size_of::<Box<[Box<str>]>>()
                    + annotations
                        .iter()
                        .flatten()
                        .map(|argument| argument.len())
                        .sum::<usize>()
                    + annotations
                        .iter()
                        .map(|arguments| arguments.len() * std::mem::size_of::<Box<str>>())
                        .sum::<usize>()
            })
            .sum()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HeaderVisibilitySuppressionApplication {
    pub annotation: Span,
    pub invisible_reference: bool,
    pub invisible_member: bool,
}

/// Compact `@Suppress` argument facts needed while signatures are solved after ordinary expression
/// arenas have been released. Annotation identity is deliberately not guessed here: the ordinary
/// resolver later joins `annotation` to its resolved classifier and accepts these flags only for
/// `kotlin.Suppress`.
#[derive(Default)]
struct HeaderVisibilitySuppressionArena {
    files: std::collections::HashMap<SourceFileId, Vec<HeaderVisibilitySuppressionApplication>>,
    declarations:
        std::collections::HashMap<DeclarationId, Vec<HeaderVisibilitySuppressionApplication>>,
}

impl HeaderVisibilitySuppressionArena {
    fn applications(
        file: &File,
        annotations: &[crate::ast::AnnotationRef],
        arguments: &[Vec<crate::ast::ExprId>],
    ) -> Vec<HeaderVisibilitySuppressionApplication> {
        annotations
            .iter()
            .zip(arguments)
            .filter_map(|(annotation, arguments)| {
                let mut invisible_reference = false;
                let mut invisible_member = false;
                for argument in arguments {
                    let Some(value) = file.const_string_value(*argument) else {
                        continue;
                    };
                    match value.as_str() {
                        Some("INVISIBLE_REFERENCE") => invisible_reference = true,
                        Some("INVISIBLE_MEMBER") => invisible_member = true,
                        _ => {}
                    }
                }
                (invisible_reference || invisible_member).then_some(
                    HeaderVisibilitySuppressionApplication {
                        annotation: annotation.span,
                        invisible_reference,
                        invisible_member,
                    },
                )
            })
            .collect()
    }

    fn add_file(&mut self, source: SourceFileId, file: &File, stubs: &[DeclarationStub]) {
        let file_applications = file
            .file_annotations
            .iter()
            .filter_map(|(annotation, arguments)| {
                Self::applications(
                    file,
                    std::slice::from_ref(annotation),
                    std::slice::from_ref(arguments),
                )
                .into_iter()
                .next()
            })
            .collect::<Vec<_>>();
        if !file_applications.is_empty() {
            self.files.insert(source, file_applications);
        }

        let mut record = |range: Span,
                          kind: DeclarationKind,
                          annotations: &[crate::ast::AnnotationRef],
                          arguments: &[Vec<crate::ast::ExprId>]| {
            let applications = Self::applications(file, annotations, arguments);
            if let (false, Some(declaration)) = (
                applications.is_empty(),
                stubs
                    .iter()
                    .find(|stub| stub.range == range && stub.kind == kind)
                    .map(|stub| stub.id),
            ) {
                self.declarations
                    .entry(declaration)
                    .or_default()
                    .extend(applications);
            }
        };
        for &declaration in &file.decls {
            match file.decl(declaration) {
                Decl::Fun(function) => record(
                    function.span,
                    DeclarationKind::Function,
                    &function.annotations,
                    &function.annotation_args,
                ),
                Decl::Property(property) => record(
                    property.span,
                    DeclarationKind::Property,
                    &property.annotations,
                    &property.annotation_args,
                ),
                Decl::Class(class) => {
                    record(
                        class.span,
                        DeclarationKind::Classifier,
                        &class.annotations,
                        &class.annotation_args,
                    );
                    if let Some(annotations) = &class.primary_ctor_annotations {
                        record(
                            class.span,
                            DeclarationKind::Constructor,
                            annotations,
                            &class.primary_ctor_annotation_args,
                        );
                    }
                    for function in &class.methods {
                        record(
                            function.span,
                            DeclarationKind::Function,
                            &function.annotations,
                            &function.annotation_args,
                        );
                    }
                    for property in &class.body_props {
                        record(
                            property.span,
                            DeclarationKind::Property,
                            &property.annotations,
                            &property.annotation_args,
                        );
                    }
                    for entry in &class.enum_entries {
                        for function in &entry.methods {
                            record(
                                function.span,
                                DeclarationKind::Function,
                                &function.annotations,
                                &function.annotation_args,
                            );
                        }
                        for property in &entry.props {
                            record(
                                property.span,
                                DeclarationKind::Property,
                                &property.annotations,
                                &property.annotation_args,
                            );
                        }
                    }
                }
            }
        }
    }

    fn file(&self, source: SourceFileId) -> &[HeaderVisibilitySuppressionApplication] {
        self.files
            .get(&source)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn declaration(&self, declaration: DeclarationId) -> &[HeaderVisibilitySuppressionApplication] {
        self.declarations
            .get(&declaration)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn storage_payload_bytes(&self) -> usize {
        self.files
            .values()
            .chain(self.declarations.values())
            .map(|applications| {
                applications.len() * std::mem::size_of::<HeaderVisibilitySuppressionApplication>()
            })
            .sum()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HeaderAnnotationArgumentRange {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HeaderAnnotationPolicyApplication {
    pub annotation: Span,
    pub arguments: HeaderAnnotationArgumentRange,
}

/// Bounded declaration metadata needed to interpret an annotation class's own `@Retention` and
/// `@Target` applications after its expression arena is gone. Only terminal enum-entry spellings
/// are copied; arbitrary annotation expressions remain ordinary checked body work.
#[derive(Default)]
struct HeaderAnnotationPolicyArena {
    applications: std::collections::HashMap<DeclarationId, Vec<HeaderAnnotationPolicyApplication>>,
    arguments: Vec<LookupNameId>,
}

impl HeaderAnnotationPolicyArena {
    fn add_class(
        &mut self,
        declaration: DeclarationId,
        class: &ClassDecl,
        file: &File,
        names: &mut LookupNames,
    ) {
        let mut applications = Vec::with_capacity(class.annotations.len());
        for (annotation, source_arguments) in class.annotations.iter().zip(&class.annotation_args) {
            let start = next_id(self.arguments.len(), "annotation policy arguments");
            for &source_argument in source_arguments {
                let entries = match file.expr(source_argument) {
                    Expr::AnnotationArrayLiteral(elements) => elements.as_slice(),
                    Expr::Call { args, .. } => args.as_slice(),
                    _ => std::slice::from_ref(&source_argument),
                };
                for &entry in entries {
                    if let Expr::Member { name, .. } = file.expr(entry) {
                        self.arguments.push(names.intern(name));
                    }
                }
            }
            let end = next_id(self.arguments.len(), "annotation policy arguments");
            applications.push(HeaderAnnotationPolicyApplication {
                annotation: annotation.span,
                arguments: HeaderAnnotationArgumentRange {
                    start,
                    len: end - start,
                },
            });
        }
        if !applications.is_empty() {
            self.applications.insert(declaration, applications);
        }
    }

    fn applications(&self, declaration: DeclarationId) -> &[HeaderAnnotationPolicyApplication] {
        self.applications
            .get(&declaration)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn arguments(&self, range: HeaderAnnotationArgumentRange) -> &[LookupNameId] {
        let start = range.start as usize;
        &self.arguments[start..start + range.len as usize]
    }

    fn storage_payload_bytes(&self) -> usize {
        self.arguments.len() * std::mem::size_of::<LookupNameId>()
            + self
                .applications
                .values()
                .map(|applications| {
                    applications.len() * std::mem::size_of::<HeaderAnnotationPolicyApplication>()
                })
                .sum::<usize>()
    }
}

/// Parse sources one at a time, expose each transient AST only for compact pass-1 extraction, and
/// drop it before parsing the next source. The callback cannot take ownership of the `File`; the
/// returned module is structurally AST-free.
///
/// This is the file-streaming primitive, not yet the complete semantic pass: expect/actual matching
/// and explicit-type resolution must be expressed over compact headers before the production driver
/// can replace `analyze_source_set_impl` with it.
pub fn stream_file_stub_inventory(
    sources: &[SourceInput<'_>],
    project_features: &LangFeatures,
    diags: &mut DiagSink,
    mut visit: impl FnMut(SourceFileId, &File, &[DeclarationStub]),
) -> StreamedHeaderModule {
    let mut builder = HeaderInventoryBuilder::default();

    for (index, source) in sources.iter().enumerate() {
        diags.set_file(index as u32);
        if source.kind == SourceKind::Java {
            builder.add_source(index, source, None);
            continue;
        }

        let mut features = project_features.clone();
        features.apply_source_directives(source.text);
        let diagnostics_before = diags.diags.len();
        let tokens = crate::lexer::lex(source.text, diags);
        let mut file = match source.kind {
            SourceKind::Kotlin => {
                crate::parser::parse_with_features(source.text, &tokens, diags, &features)
            }
            SourceKind::KotlinScript => {
                crate::parser::parse_script_with_features(source.text, &tokens, diags, &features)
            }
            SourceKind::Java => unreachable!(),
        };
        if diags.diags[diagnostics_before..]
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            continue;
        }
        if source.kind == SourceKind::Kotlin {
            if let Some(stem) = source.file_stem {
                crate::frontend::name_anonymous_classes(&mut file, &format!("{stem}Kt"));
            }
        }
        let (source_id, stubs) = builder
            .add_source(index, source, Some(&file))
            .expect("Kotlin source must produce compact headers");
        visit(source_id, &file, &stubs);
        // `file` drops here, before the next loop iteration parses another source.
    }

    builder.finish()
}

#[derive(Default)]
pub struct HeaderInventoryBuilder {
    sources: SourceMap,
    signature_origins: super::body::OriginStore,
    declarations: DeclarationIds,
    lookup_names: LookupNames,
    scopes: HeaderScopeArena,
    syntax: HeaderSyntaxArena,
    detached_types: Vec<(SourceFileId, HeaderTypeId)>,
    annotation_strings: HeaderAnnotationStringArena,
    annotation_policies: HeaderAnnotationPolicyArena,
    visibility_suppressions: HeaderVisibilitySuppressionArena,
    stubs: Vec<DeclarationStub>,
    inventory: Vec<DeclarationId>,
    local_classifier_lexical_roots: std::collections::HashMap<DeclarationId, DeclarationId>,
    inventoried: Vec<bool>,
}

impl HeaderInventoryBuilder {
    pub fn source_origin(&mut self, source: SourceFileId, span: Span) -> OriginId {
        self.signature_origins.source(source, span)
    }

    /// Add one source immediately after parsing it. Java sources receive a stable file identity but
    /// no Kotlin declaration headers. The returned stubs borrow nothing from `file`.
    pub fn add_source(
        &mut self,
        index: usize,
        source: &SourceInput<'_>,
        file: Option<&File>,
    ) -> Option<(SourceFileId, Vec<DeclarationStub>)> {
        let extension = match source.kind {
            SourceKind::Kotlin => "kt",
            SourceKind::KotlinScript => "kts",
            SourceKind::Java => "java",
        };
        let path = source.file_stem.map_or_else(
            || format!("<source-{index}>.{extension}"),
            |stem| format!("{stem}.{extension}"),
        );
        let source_id = self.sources.insert(path);
        let raw = source_id.raw() as usize;
        if self.inventoried.len() <= raw {
            self.inventoried.resize(raw + 1, false);
        }
        if source.kind == SourceKind::Java {
            assert!(file.is_none(), "Java input has no Kotlin AST header");
            return None;
        }
        let file = file?;
        self.sources.set_package(source_id, file.package.as_deref());
        self.inventoried[raw] = true;
        Some((source_id, self.add_file(source_id, file, source.is_common)))
    }

    fn add_file(
        &mut self,
        source: SourceFileId,
        file: &File,
        is_common: bool,
    ) -> Vec<DeclarationStub> {
        let first_source_type = self.syntax.type_count();
        self.scopes
            .add_file(source, file, is_common, &mut self.lookup_names);
        for ty in &file.detached_type_refs {
            let ty = self.syntax.add_type(ty, &mut self.lookup_names);
            self.detached_types.push((source, ty));
        }
        let stubs =
            extract_file_stubs(file, source, &mut self.declarations, &mut self.lookup_names);
        self.visibility_suppressions.add_file(source, file, &stubs);
        let primary_stub = |declaration: DeclId| {
            let (kind, range) = match file.decl(declaration) {
                Decl::Fun(function) => (DeclarationKind::Function, function.span),
                Decl::Property(property) => (DeclarationKind::Property, property.span),
                Decl::Class(class) => (DeclarationKind::Classifier, class.span),
            };
            stubs
                .iter()
                .find(|stub| stub.kind == kind && stub.range == range)
                .map(|stub| stub.id)
        };
        for (&local, &root) in &file.local_class_enclosing_declarations {
            if let (Some(local), Some(root)) = (primary_stub(local), primary_stub(root)) {
                self.local_classifier_lexical_roots.insert(local, root);
            }
        }
        extract_file_header_syntax(
            file,
            source,
            &mut self.declarations,
            &mut self.lookup_names,
            &mut self.syntax,
        );
        self.syntax.capture_source_spellings(
            first_source_type,
            &file.alias_spellings,
            &mut self.lookup_names,
        );
        for &declaration in &file.decls {
            match file.decl(declaration) {
                Decl::Fun(function) => {
                    let Some(stable) = stubs
                        .iter()
                        .find(|stub| {
                            stub.kind == DeclarationKind::Function
                                && stub.range == function.span
                                && self
                                    .declarations
                                    .anchor(stub.id)
                                    .is_some_and(|anchor| anchor.owner.is_none())
                        })
                        .map(|stub| stub.id)
                    else {
                        continue;
                    };
                    self.annotation_strings.add(
                        stable,
                        file,
                        &function.annotations,
                        &function.annotation_args,
                    );
                }
                Decl::Class(class) => {
                    let Some(stable) = stubs
                        .iter()
                        .find(|stub| {
                            stub.kind == DeclarationKind::Classifier && stub.range == class.span
                        })
                        .map(|stub| stub.id)
                    else {
                        continue;
                    };
                    self.annotation_strings.add(
                        stable,
                        file,
                        &class.annotations,
                        &class.annotation_args,
                    );
                    self.annotation_policies
                        .add_class(stable, class, file, &mut self.lookup_names);
                }
                Decl::Property(property) => {
                    let Some(stable) = stubs
                        .iter()
                        .find(|stub| {
                            stub.kind == DeclarationKind::Property && stub.range == property.span
                        })
                        .map(|stub| stub.id)
                    else {
                        continue;
                    };
                    self.annotation_strings.add(
                        stable,
                        file,
                        &property.annotations,
                        &property.annotation_args,
                    );
                }
            }
        }
        self.stubs.extend(stubs.iter().copied());
        self.inventory.extend(stubs.iter().map(|stub| stub.id));
        stubs
    }

    pub fn finish(self) -> StreamedHeaderModule {
        StreamedHeaderModule {
            sources: self.sources,
            signature_origins: self.signature_origins,
            declarations: self.declarations,
            lookup_names: self.lookup_names,
            scopes: self.scopes,
            syntax: self.syntax,
            detached_types: self.detached_types,
            annotation_strings: self.annotation_strings,
            annotation_policies: self.annotation_policies,
            visibility_suppressions: self.visibility_suppressions,
            stubs: self.stubs,
            inventory: self.inventory,
            local_classifier_lexical_roots: self.local_classifier_lexical_roots,
            inventoried: self.inventoried,
            excluded: std::collections::HashSet::new(),
        }
    }
}

/// Build the same AST-free Pass-1 inventory from files an existing driver has already parsed. This
/// is the production migration seam: callers can switch semantic decisions to stable headers before
/// replacing their parse loop with [`stream_file_stub_inventory`]. No AST reference enters the
/// returned value.
pub fn inventory_parsed_source_headers(
    sources: &[SourceInput<'_>],
    files: &[File],
) -> StreamedHeaderModule {
    assert_eq!(
        sources.len(),
        files.len(),
        "source/header inventory inputs must stay positionally aligned"
    );
    let mut builder = HeaderInventoryBuilder::default();
    for (index, (source, file)) in sources.iter().zip(files).enumerate() {
        let file = (source.kind != SourceKind::Java).then_some(file);
        builder.add_source(index, source, file);
    }
    builder.finish()
}

/// Match top-level multiplatform headers using only compact Pass-1 facts. The returned stable ids
/// identify expect declarations shadowed by a same-package, same-shape non-expect declaration or
/// type alias. No parser declaration id participates in the match.
pub fn matched_expect_declarations(
    headers: &StreamedHeaderModule,
) -> std::collections::HashSet<DeclarationId> {
    actualized_declaration_pairs(headers)
        .into_iter()
        .filter_map(|pair| {
            headers
                .declarations
                .anchor(pair.expect)
                .is_some_and(|anchor| anchor.owner.is_none())
                .then_some(pair.expect)
        })
        .collect()
}

/// One stable expect declaration and the actual declaration that replaces it. Descendant pairs are
/// included: an `expect class A { class B { fun f(...) } }` actualizes `A`, `A.B`, and `f` as one
/// semantic subtree even though only `A` carries the source `expect` modifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActualizedDeclarationPair {
    pub expect: DeclarationId,
    pub actual: DeclarationId,
}

/// Match actualized declaration subtrees using compact Pass-1 headers only. This is also the
/// authority for expect-owned default expressions: callers publish their presence on `actual`, but
/// retain `expect` as the stable provider identity for Pass-2 checking.
pub fn actualized_declaration_pairs(
    headers: &StreamedHeaderModule,
) -> Vec<ActualizedDeclarationPair> {
    type Key = (String, u8, String, bool, usize);
    type ActualizedAlias = (String, HeaderTypeId);

    fn type_flags_match(left: HeaderTypeFlags, right: HeaderTypeFlags) -> bool {
        left.nullable() == right.nullable()
            && left.definitely_non_null() == right.definitely_non_null()
            && left.function_receiver() == right.function_receiver()
            && left.suspend_function() == right.suspend_function()
            && left.in_projection() == right.in_projection()
            && left.out_projection() == right.out_projection()
            && left.star_projection() == right.star_projection()
    }

    fn type_shape_matches(
        headers: &StreamedHeaderModule,
        expect: HeaderTypeId,
        candidate: HeaderTypeId,
        type_parameters: &[(LookupNameId, LookupNameId)],
        actualized_aliases: &[ActualizedAlias],
    ) -> bool {
        let candidate_id = candidate;
        let (Some(expect), Some(candidate)) =
            (headers.syntax.ty(expect), headers.syntax.ty(candidate_id))
        else {
            return false;
        };
        let direct = type_flags_match(expect.flags, candidate.flags)
            && match (expect.kind, candidate.kind) {
                (
                    HeaderTypeKind::Classifier {
                        detail: expect_detail,
                        abbreviated_argument: expect_abbreviated,
                    },
                    HeaderTypeKind::Classifier {
                        detail: candidate_detail,
                        abbreviated_argument: candidate_abbreviated,
                    },
                ) => {
                    let (Some(expect_detail), Some(candidate_detail)) = (
                        headers.syntax.classifier_type(expect_detail),
                        headers.syntax.classifier_type(candidate_detail),
                    ) else {
                        return false;
                    };
                    let expect_path = headers.syntax.type_path(expect_detail.path);
                    let candidate_path = headers.syntax.type_path(candidate_detail.path);
                    let path_matches = match (expect_path, candidate_path) {
                        ([expect], [candidate]) => type_parameters
                            .iter()
                            .find(|(parameter, _)| parameter == expect)
                            .map_or_else(
                                || {
                                    headers.lookup_names.get(*expect)
                                        == headers.lookup_names.get(*candidate)
                                },
                                |(_, parameter)| parameter == candidate,
                            ),
                        _ => {
                            expect_path.len() == candidate_path.len()
                                && expect_path.iter().zip(candidate_path).all(
                                    |(expect, candidate)| {
                                        headers.lookup_names.get(*expect)
                                            == headers.lookup_names.get(*candidate)
                                    },
                                )
                        }
                    };
                    path_matches
                        && headers
                            .syntax
                            .type_operands(expect_detail.arguments)
                            .iter()
                            .zip(headers.syntax.type_operands(candidate_detail.arguments))
                            .all(|(&expect, &candidate)| {
                                type_shape_matches(
                                    headers,
                                    expect,
                                    candidate,
                                    type_parameters,
                                    actualized_aliases,
                                )
                            })
                        && headers.syntax.type_operands(expect_detail.arguments).len()
                            == headers
                                .syntax
                                .type_operands(candidate_detail.arguments)
                                .len()
                        && match (expect_abbreviated, candidate_abbreviated) {
                            (Some(expect), Some(candidate)) => type_shape_matches(
                                headers,
                                expect,
                                candidate,
                                type_parameters,
                                actualized_aliases,
                            ),
                            (None, None) => true,
                            (Some(_), None) | (None, Some(_)) => false,
                        }
                }
                (
                    HeaderTypeKind::Function {
                        parameters: expect_parameters,
                        result: expect_result,
                        context_count: expect_context_count,
                    },
                    HeaderTypeKind::Function {
                        parameters: candidate_parameters,
                        result: candidate_result,
                        context_count: candidate_context_count,
                    },
                ) => {
                    let expect_parameters = headers.syntax.type_operands(expect_parameters);
                    let candidate_parameters = headers.syntax.type_operands(candidate_parameters);
                    expect_context_count == candidate_context_count
                        && expect_parameters.len() == candidate_parameters.len()
                        && expect_parameters.iter().zip(candidate_parameters).all(
                            |(&expect, &candidate)| {
                                type_shape_matches(
                                    headers,
                                    expect,
                                    candidate,
                                    type_parameters,
                                    actualized_aliases,
                                )
                            },
                        )
                        && match (expect_result, candidate_result) {
                            (Some(expect), Some(candidate)) => type_shape_matches(
                                headers,
                                expect,
                                candidate,
                                type_parameters,
                                actualized_aliases,
                            ),
                            (None, None) => true,
                            (Some(_), None) | (None, Some(_)) => false,
                        }
                }
                (HeaderTypeKind::Classifier { .. }, HeaderTypeKind::Function { .. })
                | (HeaderTypeKind::Function { .. }, HeaderTypeKind::Classifier { .. }) => false,
            };
        if direct {
            return true;
        }
        if let HeaderTypeKind::Classifier { detail, .. } = expect.kind {
            if let Some(detail) = headers.syntax.classifier_type(detail) {
                let path = headers.syntax.type_path(detail.path);
                if path.len() == 1 {
                    let mut aliases = actualized_aliases.iter().filter(|(name, _)| {
                        headers.lookup_names.get(path[0]) == Some(name.as_str())
                    });
                    if let (Some((_, target)), None) = (aliases.next(), aliases.next()) {
                        return type_shape_matches(
                            headers,
                            *target,
                            candidate_id,
                            type_parameters,
                            actualized_aliases,
                        );
                    }
                }
            }
        }
        false
    }

    fn callable_parameter_shapes_match(
        headers: &StreamedHeaderModule,
        expect: DeclarationId,
        candidate: DeclarationId,
        actualized_aliases: &[ActualizedAlias],
    ) -> bool {
        let (
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Callable {
                        receiver: expect_receiver,
                        parameters: expect_parameters,
                        type_parameters: expect_type_parameters,
                        context_count: expect_context_count,
                        ..
                    },
                ..
            }),
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Callable {
                        receiver: candidate_receiver,
                        parameters: candidate_parameters,
                        type_parameters: candidate_type_parameters,
                        context_count: candidate_context_count,
                        ..
                    },
                ..
            }),
        ) = (
            headers.syntax.declaration(expect),
            headers.syntax.declaration(candidate),
        )
        else {
            return false;
        };
        let expect_type_parameters = headers.syntax.type_parameters(expect_type_parameters);
        let candidate_type_parameters = headers.syntax.type_parameters(candidate_type_parameters);
        if expect_type_parameters.len() != candidate_type_parameters.len()
            || expect_context_count != candidate_context_count
        {
            return false;
        }
        let type_parameters = expect_type_parameters
            .iter()
            .zip(candidate_type_parameters)
            .map(|(expect, candidate)| (expect.name, candidate.name))
            .collect::<Vec<_>>();
        let receiver_matches = match (expect_receiver, candidate_receiver) {
            (Some(expect), Some(candidate)) => type_shape_matches(
                headers,
                expect,
                candidate,
                &type_parameters,
                actualized_aliases,
            ),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        let expect_parameters = headers.syntax.parameters(expect_parameters);
        let candidate_parameters = headers.syntax.parameters(candidate_parameters);
        receiver_matches
            && expect_parameters.len() == candidate_parameters.len()
            && expect_parameters
                .iter()
                .zip(candidate_parameters)
                .all(|(expect, candidate)| {
                    expect.flags.is_vararg() == candidate.flags.is_vararg()
                        && type_shape_matches(
                            headers,
                            expect.ty,
                            candidate.ty,
                            &type_parameters,
                            actualized_aliases,
                        )
                })
    }

    fn property_input_shapes_match(
        headers: &StreamedHeaderModule,
        expect: DeclarationId,
        candidate: DeclarationId,
        actualized_aliases: &[ActualizedAlias],
    ) -> bool {
        let (
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Property {
                        receiver: expect_receiver,
                        context_parameters: expect_context,
                        type_parameters: expect_type_parameters,
                        mutable: expect_mutable,
                        ..
                    },
                ..
            }),
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Property {
                        receiver: candidate_receiver,
                        context_parameters: candidate_context,
                        type_parameters: candidate_type_parameters,
                        mutable: candidate_mutable,
                        ..
                    },
                ..
            }),
        ) = (
            headers.syntax.declaration(expect),
            headers.syntax.declaration(candidate),
        )
        else {
            return false;
        };
        if expect_mutable && !candidate_mutable {
            return false;
        }
        let expect_type_parameters = headers.syntax.type_parameters(expect_type_parameters);
        let candidate_type_parameters = headers.syntax.type_parameters(candidate_type_parameters);
        if expect_type_parameters.len() != candidate_type_parameters.len() {
            return false;
        }
        let type_parameters = expect_type_parameters
            .iter()
            .zip(candidate_type_parameters)
            .map(|(expect, candidate)| (expect.name, candidate.name))
            .collect::<Vec<_>>();
        let receiver_matches = match (expect_receiver, candidate_receiver) {
            (Some(expect), Some(candidate)) => type_shape_matches(
                headers,
                expect,
                candidate,
                &type_parameters,
                actualized_aliases,
            ),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        let expect_context = headers.syntax.parameters(expect_context);
        let candidate_context = headers.syntax.parameters(candidate_context);
        receiver_matches
            && expect_context.len() == candidate_context.len()
            && expect_context
                .iter()
                .zip(candidate_context)
                .all(|(expect, candidate)| {
                    type_shape_matches(
                        headers,
                        expect.ty,
                        candidate.ty,
                        &type_parameters,
                        actualized_aliases,
                    )
                })
    }

    fn select_actual(
        headers: &StreamedHeaderModule,
        expect: &DeclarationStub,
        candidates: &[DeclarationId],
        actualized_aliases: &[ActualizedAlias],
    ) -> Option<DeclarationId> {
        if matches!(
            expect.kind,
            DeclarationKind::Classifier | DeclarationKind::TypeAlias
        ) && candidates.len() == 1
        {
            return candidates.first().copied();
        }
        let matching = candidates
            .iter()
            .copied()
            .filter(|candidate| match expect.kind {
                DeclarationKind::Function => callable_parameter_shapes_match(
                    headers,
                    expect.id,
                    *candidate,
                    actualized_aliases,
                ),
                DeclarationKind::Property => {
                    property_input_shapes_match(headers, expect.id, *candidate, actualized_aliases)
                }
                DeclarationKind::Classifier
                | DeclarationKind::TypeAlias
                | DeclarationKind::Constructor
                | DeclarationKind::Accessor
                | DeclarationKind::Initializer
                | DeclarationKind::EnumEntry
                | DeclarationKind::Script => false,
            })
            .collect::<Vec<_>>();
        (matching.len() == 1).then(|| matching[0])
    }

    fn path(headers: &StreamedHeaderModule, range: LookupNameRange, separator: &str) -> String {
        headers
            .scopes
            .path(range)
            .iter()
            .filter_map(|name| headers.lookup_names.get(*name))
            .collect::<Vec<_>>()
            .join(separator)
    }

    fn key(headers: &StreamedHeaderModule, stub: &DeclarationStub) -> Option<Key> {
        let anchor = headers.declarations.anchor(stub.id)?;
        if anchor.owner.is_some() {
            return None;
        }
        let scope = headers.scopes.file(stub.source)?;
        let package = path(headers, scope.package, ".");
        let name = headers.lookup_names.get(stub.lookup_name?)?.to_string();
        let declaration = headers.syntax.declaration(stub.id);
        let (kind, has_receiver, arity) = match (stub.kind, declaration.map(|value| value.kind)) {
            (
                DeclarationKind::Function,
                Some(HeaderDeclarationKind::Callable {
                    receiver,
                    parameters,
                    ..
                }),
            ) => (
                0,
                receiver.is_some(),
                headers.syntax.parameters(parameters).len(),
            ),
            (DeclarationKind::Classifier, Some(HeaderDeclarationKind::Classifier { .. }))
            | (DeclarationKind::TypeAlias, Some(HeaderDeclarationKind::TypeAlias { .. })) => {
                (1, false, 0)
            }
            (DeclarationKind::Property, Some(HeaderDeclarationKind::Property { receiver, .. })) => {
                (2, receiver.is_some(), 0)
            }
            (DeclarationKind::Constructor, _)
            | (DeclarationKind::Accessor, _)
            | (DeclarationKind::Initializer, _)
            | (DeclarationKind::EnumEntry, _)
            | (DeclarationKind::Script, _)
            | (DeclarationKind::Function, _)
            | (DeclarationKind::Property, _)
            | (DeclarationKind::Classifier, _)
            | (DeclarationKind::TypeAlias, _) => return None,
        };
        Some((package, kind, name, has_receiver, arity))
    }

    let mut actuals = std::collections::HashMap::<Key, Vec<DeclarationId>>::new();
    for stub in headers
        .stubs
        .iter()
        .filter(|stub| !stub.flags.has(DeclarationFlags::EXPECT))
    {
        if let Some(key) = key(headers, stub) {
            actuals.entry(key).or_default().push(stub.id);
        }
    }
    let mut pairs = headers
        .stubs
        .iter()
        .filter(|stub| {
            stub.flags.has(DeclarationFlags::EXPECT) && stub.kind == DeclarationKind::Classifier
        })
        .filter_map(|stub| {
            let candidates = actuals.get(&key(headers, stub)?)?;
            select_actual(headers, stub, candidates, &[]).map(|actual| ActualizedDeclarationPair {
                expect: stub.id,
                actual,
            })
        })
        .collect::<Vec<_>>();
    let actualized_aliases = pairs
        .iter()
        .filter_map(|pair| {
            let expect = headers.stubs.iter().find(|stub| stub.id == pair.expect)?;
            let name = headers.lookup_names.get(expect.lookup_name?)?.to_string();
            let HeaderDeclarationKind::TypeAlias { target, .. } =
                headers.syntax.declaration(pair.actual)?.kind
            else {
                return None;
            };
            Some((name, target))
        })
        .collect::<Vec<_>>();
    pairs.extend(
        headers
            .stubs
            .iter()
            .filter(|stub| {
                stub.flags.has(DeclarationFlags::EXPECT) && stub.kind != DeclarationKind::Classifier
            })
            .filter_map(|stub| {
                let candidates = actuals.get(&key(headers, stub)?)?;
                select_actual(headers, stub, candidates, &actualized_aliases).map(|actual| {
                    ActualizedDeclarationPair {
                        expect: stub.id,
                        actual,
                    }
                })
            }),
    );

    fn child_key(
        headers: &StreamedHeaderModule,
        stub: &DeclarationStub,
    ) -> Option<(DeclarationKind, String, String, usize)> {
        let name = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
            .unwrap_or_default()
            .to_string();
        let declaration = headers.syntax.declaration(stub.id);
        let (receiver, arity) = match declaration.map(|value| value.kind) {
            Some(HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                ..
            }) => (
                receiver
                    .and_then(|ty| headers.syntax.transient_type_ref(ty, &headers.lookup_names))
                    .map(|ty| ty.name)
                    .unwrap_or_default(),
                headers.syntax.parameters(parameters).len(),
            ),
            Some(HeaderDeclarationKind::Constructor { parameters, .. }) => {
                (String::new(), headers.syntax.parameters(parameters).len())
            }
            Some(HeaderDeclarationKind::Property { receiver, .. }) => (
                receiver
                    .and_then(|ty| headers.syntax.transient_type_ref(ty, &headers.lookup_names))
                    .map(|ty| ty.name)
                    .unwrap_or_default(),
                0,
            ),
            Some(HeaderDeclarationKind::Classifier { .. })
            | Some(HeaderDeclarationKind::TypeAlias { .. })
            | None => (String::new(), 0),
        };
        Some((stub.kind, name, receiver, arity))
    }

    let mut next = 0;
    while next < pairs.len() {
        let pair = pairs[next];
        next += 1;
        let expect_children = headers.stubs.iter().filter(|stub| {
            headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner == Some(pair.expect))
        });
        let mut actual_children = std::collections::HashMap::<
            (DeclarationKind, String, String, usize),
            Vec<DeclarationId>,
        >::new();
        for child in headers.stubs.iter().filter(|stub| {
            headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner == Some(pair.actual))
        }) {
            if let Some(key) = child_key(headers, child) {
                actual_children.entry(key).or_default().push(child.id);
            }
        }
        for child in expect_children {
            let Some(key) = child_key(headers, child) else {
                continue;
            };
            let Some(candidates) = actual_children.get(&key) else {
                continue;
            };
            if candidates.len() == 1 {
                let pair = ActualizedDeclarationPair {
                    expect: child.id,
                    actual: candidates[0],
                };
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
        }
    }
    pairs
}
