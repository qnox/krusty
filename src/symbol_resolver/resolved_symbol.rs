//! The semantic result surface of one symbol-resolution query.
//!
//! Candidate collection and overload selection stay in their owning modules. These types describe
//! what the selected source name denotes so consumers can choose the facet required by their syntax
//! without repeating lookup or branching on a provider/backend origin.

use crate::libraries::{FunctionInfo, LibraryCallable, LibraryMember, PropertyInfo, SourceMember};
use crate::types::{Ty, TypeName, Visibility};

use super::{
    select_extension_property_ref, ResolvedMember, ResolvedPropertyRef, SelectedConstructorCall,
};

/// The receiver of a reference: a value, an implicit `this`, or a named type.
#[derive(Clone, Copy)]
pub enum SymRecv<'q> {
    Value(Ty),
    ImplicitValue(Ty),
    Type(&'q str),
    TypeName(TypeName),
    /// No receiver — a plain `name(args)` resolved against the import scope's top-level (and
    /// same-facade extension) functions. A dotted name is a fully-qualified reference and resolves
    /// against its own package rather than the import scope.
    TopLevel,
}

/// The facets a `recv.name` member supports. A name may denote several facets simultaneously; the
/// caller chooses the one its syntax requires after this single resolution query.
pub struct MemberFacets {
    pub call: Option<ResolvedMember>,
    pub read: Option<ResolvedMember>,
    pub write: Option<ResolvedPropertySetter>,
    pub method_ref: Option<LibraryMember>,
    pub property_ref: Option<ResolvedPropertyRef>,
    /// Receiver-less property declarations denoted by this name. Contextual overloads remain in the
    /// family because applicability depends on the caller's lexical/implicit-receiver scope.
    pub values: Vec<PropertyInfo>,
    /// Every overload applicable to the receiver, most-derived/member-first.
    pub overloads: Vec<FunctionInfo>,
    pub inaccessible_extensions: Vec<FunctionInfo>,
    /// The selected receiver-less top-level callable, ready for the emit seam.
    pub top_level_call: Option<LibraryCallable>,
    /// The selected classpath extension callable, ready for the emit seam.
    pub extension_call: Option<LibraryCallable>,
    /// The semantic result of the selected extension regardless of its physical realization.
    pub extension_result: Option<Ty>,
    pub extension_property: Option<PropertyInfo>,
}

impl MemberFacets {
    /// Materialize the callable-reference view of the already-discovered extension property.
    pub(crate) fn extension_property_ref(&self) -> Option<ResolvedPropertyRef> {
        self.extension_property
            .clone()
            .and_then(select_extension_property_ref)
    }
}

/// What a source name denotes after resolution. Consumers apply call/read/write/reference syntax to
/// this result; the resolver does not select a different declaration merely because syntax differs.
pub enum Symbol {
    Member(Box<MemberFacets>),
    Instance(LibraryMember),
    Companion(LibraryMember),
    Constructor(SelectedConstructorCall),
}

impl Symbol {
    pub(crate) fn selected_member(self) -> Option<LibraryMember> {
        match self {
            Self::Member(facets) => facets.call.map(|resolved| resolved.member),
            Self::Instance(member) | Self::Companion(member) => Some(member),
            Self::Constructor(SelectedConstructorCall::Direct(selected)) => {
                Some(selected.declaration)
            }
            Self::Constructor(SelectedConstructorCall::Platform(_)) => None,
        }
    }

    pub fn call(self) -> Option<ResolvedMember> {
        match self {
            Self::Member(facets) => facets.call,
            _ => None,
        }
    }

    pub fn call_return(self) -> Option<Ty> {
        match self {
            Self::Member(facets) => match facets.call {
                Some(call) => Some(call.ret),
                None => facets
                    .extension_call
                    .map(|call| call.ret)
                    .or(facets.extension_result),
            },
            _ => None,
        }
    }

    pub fn call_generic_sig(self) -> Option<crate::libraries::GenericSig> {
        match self {
            Self::Member(facets) => match facets.call {
                Some(call) => call.member.generic_sig,
                None => facets
                    .extension_call
                    .and_then(|call| call.generic_sig)
                    .map(|generic| *generic),
            },
            Self::Instance(member) | Self::Companion(member) => member.generic_sig,
            Self::Constructor(_) => None,
        }
    }

    pub fn property(self) -> Option<ResolvedMember> {
        match self {
            Self::Member(facets) => facets.read,
            _ => None,
        }
    }

    pub fn property_return(self) -> Option<Ty> {
        match self {
            Self::Member(facets) => match facets.read {
                Some(read) => Some(read.ret),
                None => facets.extension_property.map(|property| property.ty),
            },
            _ => None,
        }
    }

    pub fn property_setter(self) -> Option<ResolvedPropertySetter> {
        match self {
            Self::Member(facets) => facets.write,
            _ => None,
        }
    }

    pub fn method_ref(self) -> Option<LibraryMember> {
        match self {
            Self::Member(facets) => facets.method_ref,
            _ => None,
        }
    }

    pub fn property_ref(self) -> Option<ResolvedPropertyRef> {
        match self {
            Self::Member(facets) => facets.property_ref,
            _ => None,
        }
    }

    pub fn value(self) -> Option<PropertyInfo> {
        match self {
            Self::Member(mut facets) if facets.values.len() == 1 => facets.values.pop(),
            _ => None,
        }
    }

    pub fn values(self) -> Vec<PropertyInfo> {
        match self {
            Self::Member(facets) => facets.values,
            _ => Vec::new(),
        }
    }

    pub fn overloads(self) -> Vec<FunctionInfo> {
        match self {
            Self::Member(facets) => facets.overloads,
            _ => Vec::new(),
        }
    }

    pub fn top_level_call(self) -> Option<LibraryCallable> {
        match self {
            Self::Member(facets) => facets.top_level_call,
            _ => None,
        }
    }

    pub fn extension_call(self) -> Option<LibraryCallable> {
        match self {
            Self::Member(facets) => facets.extension_call,
            _ => None,
        }
    }

    pub fn extension_property_getter(self) -> Option<LibraryCallable> {
        match self {
            Self::Member(facets) => facets.extension_property.map(|property| property.getter),
            _ => None,
        }
    }

    pub fn extension_property(self) -> Option<PropertyInfo> {
        match self {
            Self::Member(facets) => facets.extension_property,
            _ => None,
        }
    }

    pub fn extension_property_ref(self) -> Option<ResolvedPropertyRef> {
        match self {
            Self::Member(facets) => facets.extension_property_ref(),
            _ => None,
        }
    }

    pub fn instance(self) -> Option<LibraryMember> {
        match self {
            Self::Instance(member) => Some(member),
            _ => None,
        }
    }

    pub fn companion(self) -> Option<LibraryMember> {
        match self {
            Self::Companion(member) => Some(member),
            _ => None,
        }
    }

    pub fn constructor(self) -> Option<SelectedConstructorCall> {
        match self {
            Self::Constructor(constructor) => Some(constructor),
            _ => None,
        }
    }
}

/// A selected property setter with the semantic access fact needed by the checker.
#[derive(Clone, Debug)]
pub struct ResolvedPropertySetter {
    pub callable: LibraryCallable,
    pub visibility: Visibility,
    pub source_member: Option<SourceMember>,
    pub stable_declaration: Option<crate::fir::DeclarationId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmbiguousExtensionProperty;

/// Binding result for a name on the compiler's error receiver.
pub(crate) enum ErrorReceiverSelection<T> {
    Absent,
    Bind(Vec<T>),
    Silent,
}
