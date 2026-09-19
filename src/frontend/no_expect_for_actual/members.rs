//! The `actual` MEMBERS of a classifier.
//!
//! A member is a declaration in its own right: it actualizes, or fails to, by its own identity,
//! and the reference compiler reports each at its own name. Which members a classifier has is a
//! question about SYNTAX — the parser hoists a nested classifier and a `companion object` out of
//! their owner, so neither rides a member list, while a primary-constructor property and a
//! secondary constructor ride lists of their own — and which of them actualized anything is a
//! question about resolved identity. Both live here, apart from the top-level check's own
//! collection, because they are answered per classifier rather than per source set.

use super::resolved::ResolvedDeclarations;
use super::{callable_modifiers, property_modifiers, render_callable, visibility};
use crate::diag::{DiagSink, Span};
use crate::resolve::Signature;
use crate::types::{Ty, Visibility};

/// One `actual` member of a classifier: where its diagnostic is reported — its own name — plus the
/// syntactic half of its rendering and the coordinate its resolved signature is keyed by.
pub(super) struct Member {
    name: Span,
    text: String,
    /// The declaration's own source range, which is what the compact header inventory anchors its
    /// identity on. A member's resolved signature is selected by THAT identity, never by spelling
    /// and arity: two overloads that tie on arity are one declaration each, and a key that cannot
    /// tell them apart leaves both unrendered.
    anchor: Span,
    /// The modality slot. A member's is not always `final`: an `override` of an `open` member
    /// renders `open`, and an interface member renders `abstract` or `open` depending on whether
    /// it has a body — both measured.
    modality: &'static str,
    kind: MemberKind,
}

enum MemberKind {
    Function {
        parameters: Vec<String>,
        type_parameters: Vec<String>,
        modifiers: String,
    },
    Property {
        is_var: bool,
        modifiers: String,
    },
    /// A SECONDARY constructor. It has no name of its own, no modality slot, and the classifier
    /// that declares it stands in for its result type.
    Constructor {
        /// Each declared parameter's name and whether it wrote `vararg` or a default — the three
        /// facts the source owns; the types come from the resolved shape.
        parameters: Vec<ConstructorParameter>,
        visibility: Visibility,
        owner: String,
    },
}

/// The syntactic half of one secondary-constructor parameter.
struct ConstructorParameter {
    name: String,
    is_vararg: bool,
    has_default: bool,
}

/// Every member of `class` that wrote `actual`, in SOURCE order.
///
/// A classifier keeps its methods, its body properties, its constructor properties and its
/// secondary constructors in four separate lists, so walking them in turn yields all of one kind
/// before any of the next — which is not the order the source wrote them in, and not the order
/// either compiler reports them in. The positions are restored here rather than left to the
/// diagnostic sink's own ordering, so this function's order is the order it claims.
pub(super) fn actual_members(class: &crate::ast::ClassDecl, owner: &str) -> Vec<Member> {
    let interface = class.kind == crate::ast::ClassKind::Interface || class.is_fun_interface;
    let mut members = Vec::new();
    for function in &class.methods {
        if !function.is_actual() {
            continue;
        }
        members.push(Member {
            name: function.name_span,
            text: function.name.clone(),
            anchor: function.span,
            modality: member_modality(
                function.is_abstract(),
                function.is_open(),
                interface,
                !matches!(function.body, crate::ast::FunBody::None),
            ),
            kind: MemberKind::Function {
                parameters: function
                    .params
                    .iter()
                    .skip(function.context_count)
                    .map(|parameter| parameter.name.clone())
                    .collect(),
                type_parameters: function.type_params.clone(),
                modifiers: callable_modifiers(function),
            },
        });
    }
    for property in &class.props {
        if !property.is_actual {
            continue;
        }
        members.push(Member {
            name: property.span,
            text: property.name.clone(),
            anchor: property.span,
            // A constructor property has no accessor body to write, and `abstract` is not legal on
            // one; only `open`/`override` move it off Kotlin's default.
            modality: member_modality(false, property.is_open, interface, true),
            kind: MemberKind::Property {
                is_var: property.is_var,
                // `external`, `const` and `lateinit` are all illegal on a constructor property,
                // so its modifier slot is always empty.
                modifiers: String::new(),
            },
        });
    }
    for property in &class.body_props {
        if !property.is_actual {
            continue;
        }
        members.push(Member {
            name: property.name_span,
            text: property.name.clone(),
            anchor: property.span,
            modality: member_modality(
                property.is_abstract,
                property.is_open,
                interface,
                property.init.is_some() || property.getter.is_some(),
            ),
            kind: MemberKind::Property {
                is_var: property.is_var,
                modifiers: property_modifiers(property),
            },
        });
    }
    for constructor in &class.secondary_ctors {
        if !constructor.is_actual {
            continue;
        }
        members.push(Member {
            // A constructor writes no name, so the reference compiler underlines the whole
            // declaration — from its first modifier through its delegation or body.
            name: constructor.declaration_span,
            text: "constructor".to_string(),
            anchor: constructor.span,
            // A constructor is never `open`, `abstract` or `override`; its rendering has no
            // modality slot at all, and this one is unread.
            modality: "final",
            kind: MemberKind::Constructor {
                parameters: constructor
                    .params
                    .iter()
                    .map(|parameter| ConstructorParameter {
                        name: parameter.name.clone(),
                        is_vararg: parameter.is_vararg,
                        has_default: parameter.default.is_some(),
                    })
                    .collect(),
                visibility: constructor.visibility,
                owner: owner.to_string(),
            },
        });
    }
    members.sort_by_key(|member| (member.name.lo, member.name.hi));
    members
}

/// Kotlin's modality for a member, as the reference compiler renders it: an interface member is
/// `abstract` with no body and `open` with one, an `override` of an `open` member is `open`, and
/// everything else is `final` unless it says otherwise.
fn member_modality(
    is_abstract: bool,
    is_open: bool,
    interface: bool,
    has_body: bool,
) -> &'static str {
    if is_abstract || (interface && !has_body) {
        "abstract"
    } else if is_open || interface {
        "open"
    } else {
        "final"
    }
}

/// Report every `actual` member of `owner` that actualized nothing.
///
/// Nothing is passed over in silence. A member whose resolved record cannot be reached is a broken
/// contract between this check and signature finalization, not a shape to skip: it is reported as
/// an internal error at the member's own name, so a member the source wrote is never dropped with
/// nothing said about it.
pub(super) fn report_members(
    members: &[Member],
    actualized: &std::collections::HashSet<crate::fir::DeclarationId>,
    declarations: &ResolvedDeclarations<'_>,
    headers: &crate::fir::StreamedHeaderModule,
    owner: Option<crate::fir::DeclarationId>,
    diags: &mut DiagSink,
) {
    let Some(class) = owner.and_then(|owner| declarations.classifier(owner)) else {
        for member in members {
            diags.error(
                member.name,
                format!(
                    "internal error: the classifier declaring actual '{}' has no resolved signature",
                    member.text
                ),
            );
        }
        return;
    };
    for member in members {
        match render_member(member, actualized, class, headers) {
            Ok(Rendered::Actualized) => {}
            Ok(Rendered::Unmatched(rendered)) => diags.error(
                member.name,
                format!("'{rendered}' has no corresponding expected declaration"),
            ),
            Err(unreachable) => diags.error(
                member.name,
                format!("internal error: actual '{}' {unreachable}", member.text),
            ),
        }
    }
}

/// What one member contributes to the report.
enum Rendered {
    /// The member actualized an `expect` member, so it has nothing to answer for.
    Actualized,
    Unmatched(String),
}

/// Render `member`, or say what about it could not be reached.
///
/// A member that DID actualize is not rendered at all: its identity is in `actualized`, which is
/// the same authority the owner's own outcome is read from.
fn render_member(
    member: &Member,
    actualized: &std::collections::HashSet<crate::fir::DeclarationId>,
    class: &crate::resolve::ClassSig,
    headers: &crate::fir::StreamedHeaderModule,
) -> Result<Rendered, &'static str> {
    let actualized_member = |stable: Option<crate::fir::DeclarationId>| {
        stable.is_some_and(|declaration| actualized.contains(&declaration))
    };
    match &member.kind {
        MemberKind::Function {
            parameters,
            type_parameters,
            modifiers,
        } => {
            let signature = member_signature(class, member, headers)
                .ok_or("has no resolved signature under its declaring classifier")?;
            if actualized_member(signature.stable_declaration) {
                return Ok(Rendered::Actualized);
            }
            render_callable(
                signature,
                member.modality,
                modifiers,
                type_parameters,
                &member.text,
                parameters,
            )
            .map(Rendered::Unmatched)
            .ok_or("has a resolved signature that did not resolve to concrete types")
        }
        MemberKind::Constructor {
            parameters,
            visibility: declared,
            owner,
        } => {
            let shape = member_constructor(class, member, headers)
                .ok_or("has no resolved shape under its declaring classifier")?;
            if actualized_member(shape.stable) {
                return Ok(Rendered::Actualized);
            }
            let rendered = parameters
                .iter()
                .enumerate()
                .map(|(index, parameter)| {
                    let declared = *shape.parameters.get(index)?;
                    // A `vararg` parameter's declared type is the ARRAY it arrives as; the
                    // reference compiler renders the element type beside the keyword.
                    let ty = if parameter.is_vararg {
                        declared.array_elem()?
                    } else {
                        declared
                    };
                    if ty.mentions_pending() {
                        return None;
                    }
                    Some(format!(
                        "{}{}: {}{}",
                        if parameter.is_vararg { "vararg " } else { "" },
                        parameter.name,
                        ty.source_name(),
                        if parameter.has_default { " = ..." } else { "" },
                    ))
                })
                .collect::<Option<Vec<_>>>()
                .ok_or("has a parameter whose type did not resolve")?
                .join(", ");
            // No modality slot, and the declaring classifier stands in for the result type.
            Ok(Rendered::Unmatched(format!(
                "{} actual constructor({rendered}): {owner}",
                visibility(*declared)
            )))
        }
        MemberKind::Property { is_var, modifiers } => {
            let property = member_property(class, member, headers)
                .ok_or("has no resolved property under its declaring classifier")?;
            if actualized_member(property.stable) {
                return Ok(Rendered::Actualized);
            }
            if property.ty.mentions_pending() {
                return Err("has a type that did not resolve");
            }
            let receiver = match property.receiver {
                // `val Tally.memberExt: Int` — the receiver binds to the name, and the
                // diagnostic still points at the NAME, not at the receiver it extends.
                Some(receiver) if !receiver.mentions_pending() => {
                    format!("{}.", receiver.source_name())
                }
                Some(_) => return Err("has an extension receiver that did not resolve"),
                None => String::new(),
            };
            Ok(Rendered::Unmatched(format!(
                "{} {} actual {modifiers}{} {receiver}{}: {}",
                visibility(property.visibility),
                member.modality,
                if *is_var { "var" } else { "val" },
                member.text,
                property.ty.source_name()
            )))
        }
    }
}

/// The resolved parameter shape of the secondary constructor this diagnostic is about, with the
/// identity that says whether it actualized anything. A classifier keeps the two in parallel
/// vectors, so the identity's position is the shape's position.
struct ResolvedMemberConstructor<'symbols> {
    stable: Option<crate::fir::DeclarationId>,
    parameters: &'symbols [Ty],
}

fn member_constructor<'symbols>(
    class: &'symbols crate::resolve::ClassSig,
    member: &Member,
    headers: &crate::fir::StreamedHeaderModule,
) -> Option<ResolvedMemberConstructor<'symbols>> {
    let position = class
        .secondary_constructor_declarations
        .iter()
        .position(|&stable| is_member(stable, member, headers))?;
    Some(ResolvedMemberConstructor {
        stable: class.secondary_constructor_declarations[position],
        parameters: class.secondary_ctor_shapes.get(position)?,
    })
}

/// Whether a resolved declaration is the source member this diagnostic is about.
///
/// The compact header inventory anchors every declaration on its own source range, and a resolved
/// signature carries that identity. Comparing it is what tells two overloads apart; a name/arity
/// key cannot, and left BOTH of a tied pair unrendered — a member the source wrote, reported
/// nowhere, with nothing saying why.
fn is_member(
    stable: Option<crate::fir::DeclarationId>,
    member: &Member,
    headers: &crate::fir::StreamedHeaderModule,
) -> bool {
    stable
        .and_then(|declaration| headers.declarations.anchor(declaration))
        .is_some_and(|anchor| anchor.range == member.anchor)
}

/// The resolved signature of the member function this diagnostic is about.
fn member_signature<'symbols>(
    class: &'symbols crate::resolve::ClassSig,
    member: &Member,
    headers: &crate::fir::StreamedHeaderModule,
) -> Option<&'symbols Signature> {
    class
        .methods
        .get(member.text.as_str())?
        .iter()
        .find(|signature| is_member(signature.stable_declaration, member, headers))
}

/// What a resolved member property contributes to its rendering, whichever of the classifier's
/// two property tables holds it. An ordinary member and a member EXTENSION property are separate
/// declarations with separate semantic records; the rendering differs only by the receiver.
struct ResolvedMemberProperty {
    /// The declaration's stable identity, which is what says whether it actualized anything.
    stable: Option<crate::fir::DeclarationId>,
    visibility: Visibility,
    ty: Ty,
    /// The declared extension receiver, for a member extension property.
    receiver: Option<Ty>,
}

/// The resolved property this diagnostic is about, by the same identity.
///
/// Both tables are consulted because a classifier keeps its ordinary members and its member
/// extension properties apart, and the source wrote one declaration either way. The identity —
/// never the name — decides which record is this member's, so a name shared between the two
/// tables cannot answer with the wrong one.
fn member_property(
    class: &crate::resolve::ClassSig,
    member: &Member,
    headers: &crate::fir::StreamedHeaderModule,
) -> Option<ResolvedMemberProperty> {
    if let Some(property) = class
        .declared_props
        .get(member.text.as_str())
        .filter(|property| is_member(property.stable_declaration, member, headers))
    {
        return Some(ResolvedMemberProperty {
            stable: property.stable_declaration,
            visibility: property.visibility,
            ty: property.ty,
            receiver: None,
        });
    }
    class
        .member_ext_props
        .get(member.text.as_str())?
        .iter()
        .find(|property| is_member(property.stable_declaration(), member, headers))
        .map(|property| ResolvedMemberProperty {
            stable: property.stable_declaration(),
            visibility: property.visibility(),
            ty: property.ret(),
            receiver: Some(property.receiver_ty()),
        })
}
