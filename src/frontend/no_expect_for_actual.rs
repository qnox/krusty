//! An `actual` declaration with no `expect` to actualize.
//!
//! `actual` is a promise that some `expect` header exists to be filled; with nothing to fill it,
//! the modifier is meaningless and Kotlin rejects the declaration. The message names the
//! declaration the way the reference compiler's declaration renderer does, so the rendering — not
//! the check — is the substance of this module.
//!
//! The rendering is a HYBRID by necessity, and the split is where each side is authoritative.
//! Source syntax owns what the declaration WROTE: its kind, name, parameter names, and which
//! parameters carry `vararg` or a default. Resolution owns every TYPE, because the reference
//! compiler renders resolved types: an inferred return (`actual fun f() = 1` → `Int`) and an
//! expanded typealias have no written form to copy.
//!
//! The two halves are also read at different TIMES, which is why the syntactic half is copied into
//! [`UnmatchedActual`] rather than looked up later. Which `actual` actualizes nothing is a
//! question about the whole source set, answerable only while every file's syntax is live; the
//! resolved types it renders are not published until Pass-1 signature finalization, by which point
//! the parser arenas may already be released.
//!
//! Every shape is measured against the reference compiler, and
//! `tests/no_expect_for_actual_e2e.rs` compares krusty's message with kotlinc's on the same source
//! rather than with a transcription. Where a shape's resolved signature cannot be reached, the
//! declaration is passed over in silence: a missing diagnostic is the behaviour that shipped, while
//! a wrongly rendered one would name a declaration the source never wrote.

use crate::ast::{Decl, DeclId, File};
use crate::diag::{DiagSink, Span};
use crate::resolve::{Signature, SymbolTable};
use crate::types::{Ty, Visibility};

/// One top-level `actual` declaration that actualizes nothing: where its diagnostic is reported —
/// the declaration's NAME, which is where the reference compiler points — plus the syntactic half
/// of its rendering.
pub(super) struct UnmatchedActual {
    file: u32,
    name: Span,
    target: Target,
    /// Whether the package-qualified name/arity key found an `expect`. This is a fallback answer
    /// only: the authority is whether actualization itself paired the declaration (see
    /// [`report`]), and a declaration with no stable identity to check against has nothing else.
    key_matched: bool,
}

enum Target {
    Function {
        declaration: DeclId,
        name: String,
        /// Value-parameter names in declaration order, excluding the extension receiver.
        parameters: Vec<String>,
        type_parameters: Vec<String>,
        /// The modifier words between `actual` and `fun`, already in the reference compiler's
        /// order — they are what the declaration WROTE, so syntax owns them.
        modifiers: String,
    },
    Property {
        declaration: DeclId,
        name: String,
        /// `const ` / `lateinit `, between `actual` and `val`/`var`.
        modifiers: String,
    },
    Classifier {
        declaration: DeclId,
        name: String,
        /// The syntactic half of a classifier's rendering: its kind, modality, and the modifier
        /// words that sit between `actual` and the kind keyword.
        shape: ClassifierShape,
        /// The classifier's own `actual` members, reported only when the classifier itself is —
        /// see [`report`].
        members: Vec<Member>,
    },
    /// A declaration whose rendering is not derivable — see [`collect`].
    Unrenderable,
    TypeAlias {
        /// Fully-qualified internal name (`pkg/Alias`), which is how the alias's resolved
        /// expansion is keyed.
        qualified: String,
        name: String,
        visibility: Visibility,
    },
}

/// Every top-level `actual` in the source set, with the syntactic half of its rendering and the
/// answer of the name/arity key. Whether each one actualized anything is settled in [`report`].
pub(super) fn collect(files: &[File]) -> Vec<UnmatchedActual> {
    let expects = files
        .iter()
        .flat_map(|file| {
            file.expect_decls
                .iter()
                .map(move |expect| super::expect_key(file, expect.declaration))
        })
        .collect::<std::collections::HashSet<_>>();
    let mut unmatched = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let package = file.package.clone().unwrap_or_default();
        for &declaration in &file.actual_decls {
            let key_matched = expects.contains(&super::expect_key(file, declaration));
            let (name, target) = match file.decl(declaration) {
                Decl::Fun(function) => (
                    function.name_span,
                    Target::Function {
                        declaration,
                        name: function.name.clone(),
                        // Context parameters lead the resolved list and are not part of the
                        // declaration's own; they are dropped with the receiver below.
                        parameters: function
                            .params
                            .iter()
                            .skip(function.context_count)
                            .map(|parameter| parameter.name.clone())
                            .collect(),
                        type_parameters: function.type_params.clone(),
                        modifiers: callable_modifiers(function),
                    },
                ),
                Decl::Property(property) => (
                    property.name_span,
                    // A context parameter's rendering is not measured, so such a property is
                    // passed over rather than rendered by guess.
                    if property.context_params.is_empty() {
                        Target::Property {
                            declaration,
                            name: property.name.clone(),
                            modifiers: property_modifiers(property),
                        }
                    } else {
                        Target::Unrenderable
                    },
                ),
                Decl::Class(class) => (
                    class.name_span,
                    Target::Classifier {
                        declaration,
                        name: class.name.clone(),
                        shape: ClassifierShape::of(class),
                        members: actual_members(class),
                    },
                ),
            };
            unmatched.push(UnmatchedActual {
                file: index as u32,
                name,
                target,
                key_matched,
            });
        }
        for &alias in &file.actual_type_aliases {
            let Some(declaration) = file.type_alias_decls.get(alias) else {
                continue;
            };
            // An alias actualizes an `expect class`, so it answers the classifier key.
            let key = (
                package.clone(),
                1,
                declaration.name.clone(),
                String::new(),
                0,
            );
            if expects.contains(&key) {
                continue;
            }
            unmatched.push(UnmatchedActual {
                file: index as u32,
                name: declaration.name_span,
                target: Target::TypeAlias {
                    qualified: if package.is_empty() {
                        declaration.name.clone()
                    } else {
                        format!("{package}/{}", declaration.name)
                    },
                    name: declaration.name.clone(),
                    visibility: file
                        .type_alias_visibility
                        .get(&declaration.name)
                        .copied()
                        .unwrap_or(Visibility::Public),
                },
                key_matched: false,
            });
        }
    }
    unmatched
}

impl UnmatchedActual {
    /// Whether this `actual` actualized something after all.
    fn actualized(
        &self,
        actualized: &std::collections::HashSet<crate::fir::DeclarationId>,
        symbols: &SymbolTable,
    ) -> bool {
        let stable = match &self.target {
            Target::Function { declaration, .. } => {
                function_signature(symbols, self.file, *declaration)
                    .and_then(|signature| signature.stable_declaration)
            }
            Target::Property { declaration, .. } => {
                property_stable_declaration(symbols, self.file, *declaration)
            }
            Target::Classifier { declaration, .. } => {
                classifier_signature(symbols, self.file, *declaration)
                    .and_then(|signature| signature.stable_declaration)
            }
            // An alias is matched by name; see [`collect`].
            Target::TypeAlias { .. } | Target::Unrenderable => return self.key_matched,
        };
        match stable {
            Some(stable) => actualized.contains(&stable),
            None => self.key_matched,
        }
    }
}

/// Report every `actual` that actualized nothing.
///
/// `actualized` is the set of declarations actualization PAIRED with an `expect`, and it — not the
/// name/arity key — is the authority: that matcher compares resolved type shapes and follows an
/// `actual typealias`, so it pairs `expect val S.tag: S` with `actual val String.tag: String`
/// where the key cannot. The key's answer stands in only for a declaration that resolution gave
/// no stable identity to look up.
pub(super) fn report(
    unmatched: &[UnmatchedActual],
    actualized: &std::collections::HashSet<crate::fir::DeclarationId>,
    symbols: &SymbolTable,
    diags: &mut DiagSink,
) {
    for actual in unmatched {
        if actual.actualized(actualized, symbols) {
            continue;
        }
        let rendered = match &actual.target {
            Target::Function {
                declaration,
                name,
                parameters,
                type_parameters,
                modifiers,
            } => render_function(
                symbols,
                actual.file,
                *declaration,
                name,
                parameters,
                type_parameters,
                modifiers,
            ),
            Target::Property {
                declaration,
                name,
                modifiers,
            } => render_property(symbols, actual.file, *declaration, name, modifiers),
            Target::Classifier {
                declaration,
                name,
                shape,
                members,
            } => {
                let rendered = render_classifier(symbols, actual.file, *declaration, name, shape);
                // A member of an `actual` classifier that actualizes nothing cannot itself have
                // actualized anything, so the reference compiler reports both — each at its own
                // name. The converse (an owner that DID match, with a member that did not) needs
                // member-level matching, which actualization does not do: its pairing is
                // top-level only and a matched classifier's members are excluded as a subtree
                // rather than paired. Those stay silent; see `docs/PARITY_PROTOCOL.md`.
                if rendered.is_some() {
                    report_members(members, symbols, actual.file, *declaration, diags);
                }
                rendered
            }
            Target::Unrenderable => None,
            Target::TypeAlias {
                qualified,
                name,
                visibility,
            } => render_type_alias(symbols, qualified, name, *visibility),
        };
        let Some(rendered) = rendered else {
            continue;
        };
        diags.set_file(actual.file);
        diags.error(
            actual.name,
            format!("'{rendered}' has no corresponding expected declaration"),
        );
    }
}

/// The modifier words a function writes between `actual` and `fun`, in the reference compiler's
/// measured order: `external` precedes `override`, and `inline` precedes `operator`, `infix` and
/// `suspend`. Each was measured alone and the ordering pairs that are legal Kotlin were measured
/// together; see `docs/PARITY_PROTOCOL.md`.
fn callable_modifiers(function: &crate::ast::FunDecl) -> String {
    [
        ("external", function.is_external()),
        ("override", function.is_override()),
        ("inline", function.is_inline()),
        ("operator", function.is_operator()),
        ("infix", function.is_infix()),
        ("tailrec", function.is_tailrec()),
        ("suspend", function.is_suspend()),
    ]
    .into_iter()
    .filter_map(|(word, written)| written.then_some(word))
    .fold(String::new(), |mut words, word| {
        words.push_str(word);
        words.push(' ');
        words
    })
}

/// The same for a property, between `actual` and `val`/`var`. `const` and `lateinit` cannot both
/// be written (one implies `val`, the other `var`), so their relative order never arises.
fn property_modifiers(property: &crate::ast::PropDecl) -> String {
    [
        ("external", property.is_external),
        ("const", property.is_const),
        ("lateinit", property.is_lateinit),
    ]
    .into_iter()
    .filter_map(|(word, written)| written.then_some(word))
    .fold(String::new(), |mut words, word| {
        words.push_str(word);
        words.push(' ');
        words
    })
}

/// `public`/`internal`/`protected`/`private`, the way the reference compiler spells it.
fn visibility(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Internal => "internal",
        Visibility::Protected => "protected",
        Visibility::Private => "private",
        // A Java-origin package-private declaration cannot carry `actual`, so `public` is the only
        // remaining Kotlin spelling.
        Visibility::Public | Visibility::PackagePrivate => "public",
    }
}

/// `<T, U : Bound> `, or nothing when the declaration is not generic. A bound is rendered only
/// where one was declared: a bare `<T>` carries Kotlin's implicit `Any?`, which the reference
/// compiler omits.
fn type_parameters(names: &[String], bound: &dyn Fn(usize) -> Option<Ty>) -> String {
    if names.is_empty() {
        return String::new();
    }
    let rendered = names
        .iter()
        .enumerate()
        .map(|(index, name)| match bound(index) {
            Some(bound) => format!("{name} : {}", bound.source_name()),
            None => name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{rendered}> ")
}

#[allow(clippy::too_many_arguments)]
fn render_function(
    symbols: &SymbolTable,
    file: u32,
    declaration: DeclId,
    name: &str,
    parameters: &[String],
    formals: &[String],
    modifiers: &str,
) -> Option<String> {
    let signature = function_signature(symbols, file, declaration)?;
    // A top-level declaration is always `final`.
    render_callable(signature, "final", modifiers, formals, name, parameters)
}

/// Render one callable — top-level or member — from its resolved signature and the syntax it wrote.
fn render_callable(
    signature: &Signature,
    modality: &str,
    modifiers: &str,
    formals: &[String],
    name: &str,
    parameters: &[String],
) -> Option<String> {
    // A generic callable's `params`/`ret` are ERASED (`fun <T> id(t: T): T` keys as `Any`); its
    // declared shape lives on the generic signature, which is what the source spells and what the
    // reference compiler renders.
    let generic = signature.generic_sig.as_ref();
    let declared_types: &[Ty] =
        generic.map_or(&signature.params[..], |generic| &generic.params[..]);
    let result = generic.map_or(signature.ret, |generic| generic.ret);
    if result.mentions_pending() {
        return None;
    }
    let receiver = match generic
        .and_then(|generic| generic.receiver)
        .or(signature.source_receiver)
    {
        Some(receiver) => format!("{}.", receiver.source_name()),
        None => String::new(),
    };
    // The extension receiver and any context parameters occupy LEADING slots of the resolved
    // parameter list; the rendered list is the declaration's own, so they are skipped rather than
    // named.
    let leading = declared_types.len().checked_sub(parameters.len())?;
    let rendered = parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let slot = index + leading;
            let declared = *declared_types.get(slot)?;
            let vararg = signature.vararg_index == Some(slot);
            // A `vararg` parameter's declared type is the ARRAY it arrives as; the reference
            // compiler renders the element type beside the keyword.
            let ty = if vararg {
                declared.array_elem()?
            } else {
                declared
            };
            if ty.mentions_pending() {
                return None;
            }
            // The presence of a default is rendered, never its value — literally `...`.
            let default = if signature.param_defaults.get(slot).copied().unwrap_or(false) {
                " = ..."
            } else {
                ""
            };
            Some(format!(
                "{}{parameter}: {}{default}",
                if vararg { "vararg " } else { "" },
                ty.source_name()
            ))
        })
        .collect::<Option<Vec<_>>>()?
        .join(", ");
    let formals = type_parameters(formals, &|index| declared_formal_bound(signature, index));
    Some(format!(
        "{} {modality} actual {modifiers}fun {formals}{receiver}{name}({rendered}): {}",
        visibility(signature.visibility),
        result.source_name()
    ))
}

/// The resolved signature of a top-level source function. A top-level EXTENSION function lives in
/// its own receiver-keyed table rather than beside the ordinary overloads, so both are searched by
/// the declaration that produced them.
fn function_signature(symbols: &SymbolTable, file: u32, declaration: DeclId) -> Option<&Signature> {
    let owns = |candidate: &&Signature| {
        candidate.source_file == Some(file) && candidate.source_decl == Some(declaration)
    };
    symbols.funs.values().flatten().find(owns).or_else(|| {
        symbols
            .ext_funs
            .values()
            .flat_map(|receivers| receivers.values())
            .flatten()
            .find(owns)
    })
}

/// The stable identity of a top-level source property, plain or extension.
fn property_stable_declaration(
    symbols: &SymbolTable,
    file: u32,
    declaration: DeclId,
) -> Option<crate::fir::DeclarationId> {
    if let Some(signature) = symbols.source_props.get(&(file, declaration.0)) {
        return signature.stable_declaration;
    }
    symbols
        .ext_props
        .values()
        .flatten()
        .find(|candidate| candidate.source == (file, declaration.0))
        .and_then(|signature| signature.stable_declaration)
}

fn classifier_signature(
    symbols: &SymbolTable,
    file: u32,
    declaration: DeclId,
) -> Option<&crate::resolve::ClassSig> {
    symbols.classes.values().find(|candidate| {
        candidate.source_file == file && candidate.source_decl == Some(declaration)
    })
}

/// The DECLARED upper bound of a callable's type parameter, or `None` for Kotlin's implicit `Any?`
/// (which the reference compiler leaves unwritten).
fn declared_formal_bound(signature: &Signature, index: usize) -> Option<Ty> {
    let generic = signature.generic_sig.as_ref()?;
    let bounds = generic.formal_bounds.get(index)?;
    bounds.first().copied().and_then(declared_bound)
}

/// Kotlin's implicit upper bound is nullable `Any`, which the reference compiler never writes; any
/// other bound it renders. Providers spell the implicit one either as an absent bound or as `Any?`,
/// so both are folded here.
fn declared_bound(bound: Ty) -> Option<Ty> {
    (bound != Ty::nullable(Ty::obj("kotlin/Any"))).then_some(bound)
}

fn render_property(
    symbols: &SymbolTable,
    file: u32,
    declaration: DeclId,
    name: &str,
    modifiers: &str,
) -> Option<String> {
    if let Some(signature) = symbols.source_props.get(&(file, declaration.0)) {
        if signature.ty.mentions_pending() {
            return None;
        }
        return Some(format!(
            "{} final actual {modifiers}{} {name}: {}",
            visibility(signature.visibility),
            if signature.is_var { "var" } else { "val" },
            signature.ty.source_name()
        ));
    }
    // An extension property lives in its own receiver-keyed table, which is also the only place
    // its receiver survives resolution.
    let signature = symbols
        .ext_props
        .values()
        .flatten()
        .find(|candidate| candidate.source == (file, declaration.0))?;
    if signature.ty.mentions_pending() || signature.receiver.mentions_pending() {
        return None;
    }
    let formals = if signature.formal_names.is_empty() {
        &signature.formals
    } else {
        &signature.formal_names
    };
    let formals = type_parameters(formals, &|index| {
        signature
            .formal_bounds
            .get(index)
            .copied()
            .and_then(declared_bound)
    });
    Some(format!(
        "{} final actual {modifiers}{} {formals}{}.{name}: {}",
        visibility(signature.visibility),
        if signature.is_var { "var" } else { "val" },
        signature.receiver.source_name(),
        signature.ty.source_name()
    ))
}

/// The syntactic half of a classifier's rendering, copied out while its syntax is live.
struct ClassifierShape {
    kind: crate::ast::ClassKind,
    modality: crate::ast::Modality,
    singleton: bool,
    is_data: bool,
    is_value: bool,
    is_inner: bool,
    is_fun_interface: bool,
}

impl ClassifierShape {
    fn of(class: &crate::ast::ClassDecl) -> Self {
        Self {
            kind: class.kind,
            modality: class.modality,
            singleton: class.singleton,
            is_data: class.is_data,
            is_value: class.is_value,
            is_inner: class.inner_of.is_some(),
            is_fun_interface: class.is_fun_interface,
        }
    }

    /// The modality slot. Kotlin's default is `final`, except that an interface is `abstract`
    /// unless it declares a stronger modality — `sealed interface` renders `sealed` (measured).
    fn modality(&self) -> &'static str {
        match self.modality {
            crate::ast::Modality::Sealed => "sealed",
            crate::ast::Modality::Abstract => "abstract",
            crate::ast::Modality::Open => "open",
            crate::ast::Modality::Final
                if self.kind == crate::ast::ClassKind::Interface || self.is_fun_interface =>
            {
                "abstract"
            }
            crate::ast::Modality::Final => "final",
        }
    }

    /// The modifier words and kind keyword, in the reference compiler's order — `inner` precedes
    /// `data` (measured: `public final actual inner data class InnerData : Any`), and `enum` /
    /// `annotation` belong to the kind keyword itself.
    fn keyword(&self) -> String {
        let mut words = Vec::new();
        if self.is_inner {
            words.push("inner");
        }
        if self.is_data {
            words.push("data");
        }
        if self.is_value {
            words.push("value");
        }
        if self.is_fun_interface {
            words.push("fun");
        }
        words.push(match self.kind {
            crate::ast::ClassKind::Interface => "interface",
            crate::ast::ClassKind::Enum => "enum class",
            crate::ast::ClassKind::Annotation => "annotation class",
            crate::ast::ClassKind::Class if self.singleton => "object",
            crate::ast::ClassKind::Class => "class",
        });
        words.join(" ")
    }
}

fn render_classifier(
    symbols: &SymbolTable,
    file: u32,
    declaration: DeclId,
    name: &str,
    shape: &ClassifierShape,
) -> Option<String> {
    let signature = classifier_signature(symbols, file, declaration)?;
    // A classifier's type parameters are stored as SEMANTIC identities; their source spelling is
    // what the declaration wrote and what the reference compiler renders.
    let names = signature
        .type_params()
        .iter()
        .map(|parameter| crate::types::type_parameter_source_name(parameter).to_string())
        .collect::<Vec<_>>();
    let formals = type_parameters(&names, &|index| {
        signature
            .type_param_bounds()
            .get(index)
            .copied()
            .and_then(declared_bound)
    })
    // A classifier's type parameters bind directly to its name — `class Generic<T> : Any`, with no
    // space before the `<` that a callable's leading `<T> ` keeps.
    .trim_end()
    .to_string();
    // The reference compiler always prints a supertype, including the one Kotlin supplies: `Any`
    // for an ordinary classifier, and the classifier's own superclass for an enum or an annotation
    // class, neither of which krusty models as a declared supertype.
    let supertypes = match shape.kind {
        crate::ast::ClassKind::Enum => format!("Enum<{name}>"),
        crate::ast::ClassKind::Annotation => "Annotation".to_string(),
        _ => {
            let base = signature
                .super_internal_name()
                .map(|base| Ty::obj_args_name(base, &signature.super_type_args));
            let interfaces = signature
                .interface_names()
                .enumerate()
                .map(|(index, interface)| {
                    Ty::obj_args_name(
                        interface,
                        signature
                            .interface_type_args
                            .get(index)
                            .map_or(&[][..], |arguments| &arguments[..]),
                    )
                });
            let rendered = base
                .into_iter()
                .chain(interfaces)
                .map(Ty::source_name)
                .collect::<Vec<_>>();
            if rendered.is_empty() {
                "Any".to_string()
            } else {
                rendered.join(", ")
            }
        }
    };
    Some(format!(
        "{} {} actual {} {name}{formals} : {supertypes}",
        visibility(signature.visibility),
        shape.modality(),
        shape.keyword()
    ))
}

fn render_type_alias(
    symbols: &SymbolTable,
    qualified: &str,
    name: &str,
    declared: Visibility,
) -> Option<String> {
    let (formals, target) = symbols
        .source_alias_expansions
        .get(&crate::types::type_name(qualified))?;
    if target.mentions_pending() {
        return None;
    }
    let formals = if formals.is_empty() {
        String::new()
    } else {
        format!("<{}>", formals.join(", "))
    };
    Some(format!(
        "{} final actual typealias {name}{formals} = {}",
        visibility(declared),
        target.source_name()
    ))
}

/// One `actual` member of a classifier: where its diagnostic is reported — its own name — plus the
/// syntactic half of its rendering and the coordinate its resolved signature is keyed by.
struct Member {
    name: Span,
    text: String,
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
}

/// Every member of `class` that wrote `actual`, in declaration order.
fn actual_members(class: &crate::ast::ClassDecl) -> Vec<Member> {
    let interface = class.kind == crate::ast::ClassKind::Interface || class.is_fun_interface;
    let mut members = Vec::new();
    for function in &class.methods {
        if !function.is_actual() {
            continue;
        }
        members.push(Member {
            name: function.name_span,
            text: function.name.clone(),
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
    for property in &class.body_props {
        if !property.is_actual {
            continue;
        }
        members.push(Member {
            name: property.name_span,
            text: property.name.clone(),
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

fn report_members(
    members: &[Member],
    symbols: &SymbolTable,
    file: u32,
    owner: DeclId,
    diags: &mut DiagSink,
) {
    let Some(class) = classifier_signature(symbols, file, owner) else {
        return;
    };
    for member in members {
        let Some(rendered) = render_member(member, class) else {
            continue;
        };
        diags.error(
            member.name,
            format!("'{rendered}' has no corresponding expected declaration"),
        );
    }
}

fn render_member(member: &Member, class: &crate::resolve::ClassSig) -> Option<String> {
    match &member.kind {
        MemberKind::Function {
            parameters,
            type_parameters,
            modifiers,
        } => {
            let signature = member_signature(class, &member.text, parameters.len())?;
            render_callable(
                signature,
                member.modality,
                modifiers,
                type_parameters,
                &member.text,
                parameters,
            )
        }
        MemberKind::Property { is_var, modifiers } => {
            let property = class.declared_props.get(member.text.as_str())?;
            if property.ty.mentions_pending() {
                return None;
            }
            Some(format!(
                "{} {} actual {modifiers}{} {}: {}",
                visibility(property.visibility),
                member.modality,
                if *is_var { "var" } else { "val" },
                member.text,
                property.ty.source_name()
            ))
        }
    }
}

/// The resolved signature of one of `class`'s member functions.
///
/// It is found by NAME and arity, not by an AST coordinate: a member signature collected through
/// the streaming pass leaves `Signature::source_member` unset (that field records a SELECTED
/// member handed to lowering, not where a declaration came from). Overloads that tie on arity are
/// left unrendered rather than guessed — a wrong rendering names a declaration the source never
/// wrote, while a missing one is the behaviour that shipped.
fn member_signature<'symbols>(
    class: &'symbols crate::resolve::ClassSig,
    name: &str,
    arity: usize,
) -> Option<&'symbols Signature> {
    let mut matching = class
        .methods
        .get(name)?
        .iter()
        .filter(|signature| signature.params.len() == arity);
    match (matching.next(), matching.next()) {
        (Some(signature), None) => Some(signature),
        _ => None,
    }
}
