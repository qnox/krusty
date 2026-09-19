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
//! rather than with a transcription.
//!
//! Whether an `actual` actualized anything is decided by ACTUALIZATION'S OWN pairing, read by the
//! declaration's stable identity and by nothing else. There is no second authority: a name/arity
//! key differs on a receiver's spelling exactly where actualization follows an `actual typealias`,
//! so consulting it reported pairs that had matched. Nothing is passed over in silence either — a
//! declaration this check can find no identity or no rendering for reports an internal error at
//! its own name, because a declaration the source wrote disappearing is the failure mode a
//! diagnostic pass cannot have.
//!
//! A classifier's `actual` MEMBERS are a question of their own — a member actualizes by its own
//! identity, and the syntax it is found in is not the syntax the classifier itself is found in —
//! and they live in [`members`].

mod members;
mod resolved;

use crate::ast::{Decl, File};
use crate::diag::{DiagSink, Span};
use crate::resolve::{Signature, SymbolTable};
use crate::types::{Ty, Visibility};
use members::{actual_members, report_members, Member};
use resolved::{Resolved, ResolvedDeclarations};

/// One top-level `actual` declaration that actualizes nothing: where its diagnostic is reported —
/// the declaration's NAME, which is where the reference compiler points — plus the syntactic half
/// of its rendering.
pub(super) struct UnmatchedActual {
    file: u32,
    name: Span,
    /// The declaration's own source range, which is the coordinate the compact header inventory
    /// anchors its stable identity on. It is how an `actual typealias` is identified — an alias
    /// has no resolved signature to carry an identity for it.
    anchor: Span,
    target: Target,
}

enum Target {
    Function {
        name: String,
        /// Context-parameter names in declaration order. They occupy the LEADING slots of the
        /// resolved parameter list, which is where their types come from; only the names are the
        /// declaration's own.
        context_parameters: Vec<String>,
        /// Value-parameter names in declaration order, excluding the extension receiver.
        parameters: Vec<String>,
        type_parameters: Vec<String>,
        /// The modifier words between `actual` and `fun`, already in the reference compiler's
        /// order — they are what the declaration WROTE, so syntax owns them.
        modifiers: String,
    },
    Property {
        name: String,
        /// `const ` / `lateinit `, between `actual` and `val`/`var`.
        modifiers: String,
    },
    Classifier {
        name: String,
        /// The syntactic half of a classifier's rendering: its kind, modality, and the modifier
        /// words that sit between `actual` and the kind keyword.
        shape: ClassifierShape,
        /// The classifier's own `actual` members, reported only when the classifier itself is —
        /// see [`report`].
        members: Vec<Member>,
    },
    TypeAlias {
        /// Fully-qualified internal name (`pkg/Alias`), which is how the alias's resolved
        /// expansion is keyed.
        qualified: String,
        name: String,
        visibility: Visibility,
    },
}

/// Every `actual` in the source set — top-level and hoisted nested classifiers alike — with the
/// syntactic half of its rendering. Whether each one actualized anything is settled in [`report`],
/// from its stable declaration identity and nothing else.
/// Report every top-level implementation the source wrote WITHOUT `actual`.
///
/// A declaration sharing an `expect`'s package, kind, name, receiver and arity is the
/// implementation that header was written for. The reference compiler says so at the declaration's
/// own name and leaves the `expect` matched, rather than reporting the header as unfilled — the
/// coincidence is an error about the implementation, and treating it as a valid one instead let a
/// declaration that never claimed anything inherit the `expect`'s defaults in silence.
///
/// Reported while every file's syntax is still live, because the name is the coordinate and the
/// compact inventory anchors only the declaration's whole range.
pub(super) fn report_unmarked_implementations(
    unmarked: &[crate::fir::DeclarationId],
    files: &[File],
    headers: &crate::fir::StreamedHeaderModule,
    diags: &mut DiagSink,
) {
    if unmarked.is_empty() {
        return;
    }
    let mut names = std::collections::HashMap::new();
    for (index, file) in files.iter().enumerate() {
        for &declaration in &file.decls {
            let (name, span) = match file.decl(declaration) {
                Decl::Fun(function) => (function.name_span, function.span),
                Decl::Property(property) => (property.name_span, property.span),
                Decl::Class(class) => (class.name_span, class.span),
            };
            names.insert((index as u32, span.lo, span.hi), name);
        }
        for alias in &file.type_alias_decls {
            names.insert(
                (index as u32, alias.span.lo, alias.span.hi),
                alias.name_span,
            );
        }
    }
    for &declaration in unmarked {
        let Some(anchor) = headers.declarations.anchor(declaration) else {
            continue;
        };
        let file = anchor.source.raw();
        let Some(&name) = names.get(&(file, anchor.range.lo, anchor.range.hi)) else {
            continue;
        };
        diags.set_file(file);
        diags.error(
            name,
            "declaration must be marked with 'actual'.".to_string(),
        );
    }
}

pub(super) fn collect(files: &[File]) -> Vec<UnmatchedActual> {
    let mut unmatched = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let file_start = unmatched.len();
        let package = file.package.clone().unwrap_or_default();
        // Which hoisted classifier is a `companion object` is an edge its OWNER records; the
        // companion itself is an ordinary singleton declaration. The reference compiler renders
        // the word, so the edge is read back here rather than guessed from the name `Companion`,
        // which `companion object Named` does not write.
        let companions = file
            .decls
            .iter()
            .filter_map(|&owner| match file.decl(owner) {
                Decl::Class(class) => class.companion,
                Decl::Fun(_) | Decl::Property(_) => None,
            })
            .collect::<std::collections::HashSet<_>>();
        for &declaration in &file.actual_decls {
            let (name, anchor, target) = match file.decl(declaration) {
                Decl::Fun(function) => (
                    function.name_span,
                    function.span,
                    Target::Function {
                        name: function.name.clone(),
                        context_parameters: function
                            .params
                            .iter()
                            .take(function.context_count)
                            .map(|parameter| parameter.name.clone())
                            .collect(),
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
                    property.span,
                    Target::Property {
                        name: property.name.clone(),
                        modifiers: property_modifiers(property),
                    },
                ),
                Decl::Class(class) => (
                    class.name_span,
                    class.span,
                    Target::Classifier {
                        // A nested classifier is hoisted under its owner's path (`Holder.Inner`),
                        // and the reference compiler renders the declaration's own simple name.
                        name: simple_name(&class.name).to_string(),
                        shape: ClassifierShape::of(class, companions.contains(&declaration)),
                        members: actual_members(class, simple_name(&class.name)),
                    },
                ),
            };
            unmatched.push(UnmatchedActual {
                file: index as u32,
                name,
                anchor,
                target,
            });
        }
        for &alias in &file.actual_type_aliases {
            let Some(declaration) = file.type_alias_decls.get(alias) else {
                continue;
            };
            unmatched.push(UnmatchedActual {
                file: index as u32,
                name: declaration.name_span,
                anchor: declaration.span,
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
            });
        }
        // A file's `actual` declarations and its `actual typealias`es are two lists, and a report
        // that emptied one after the other put every alias last however the source interleaved
        // them. Diagnostic order is source order, so they are merged on the one coordinate both
        // carry — the declaration's own range.
        unmatched[file_start..].sort_by_key(|entry| (entry.anchor.lo, entry.anchor.hi));
    }
    unmatched
}

/// Report every `actual` that actualized nothing.
///
/// `actualized` is the set of declarations actualization PAIRED with an `expect`, keyed by stable
/// declaration identity — the only authority. That matcher compares resolved type shapes and
/// follows an `actual typealias`, so it pairs `expect val S.tag: S` with
/// `actual val String.tag: String` where a name/arity key differing on the receiver spelling
/// cannot; consulting such a key as a second answer reported pairs that had matched.
pub(super) fn report(
    unmatched: &[UnmatchedActual],
    actualized: &std::collections::HashSet<crate::fir::DeclarationId>,
    symbols: &SymbolTable,
    headers: &crate::fir::StreamedHeaderModule,
    diags: &mut DiagSink,
) {
    // Published once, and only looked up below: this pass never walks a resolved table itself.
    let declarations = ResolvedDeclarations::publish(symbols);
    for actual in unmatched {
        // Every diagnostic this iteration writes belongs to this declaration's file, members
        // included. Setting it only before the owner's own error left a member's inheriting
        // whichever file was active last, which in a single-file test is invisible and across files
        // points the message at the wrong source.
        diags.set_file(actual.file);
        // The owner's own diagnostic is written BEFORE its members', which is the order the source
        // declares them in — the declaration opens the body its members live in.
        //
        // Nothing is passed over in silence: an `actual` this check cannot answer for reports an
        // internal error at its own name, so a declaration the source wrote never disappears
        // because a lookup returned nothing.
        let stable = actual.stable(headers);
        match stable
            .ok_or("has no stable declaration identity")
            .and_then(|stable| {
                if actualized.contains(&stable) {
                    Ok(None)
                } else {
                    actual.render(stable, &declarations).map(Some)
                }
            }) {
            Ok(None) => {}
            Ok(Some(rendered)) => diags.error(
                actual.name,
                format!("'{rendered}' has no corresponding expected declaration"),
            ),
            Err(unreachable) => diags.error(
                actual.name,
                format!("internal error: this actual declaration {unreachable}"),
            ),
        }
        // A member is a declaration of its own: it actualizes, or fails to, by its own identity.
        // Actualization pairs a matched owner's members individually (see
        // `fir::actualized_declaration_pairs`), so the owner's own outcome decides only the
        // owner's diagnostic — an unmatched member under a MATCHED owner is reported here just
        // the same, and so is every member of an owner whose own rendering could not be produced.
        if let Target::Classifier { members, .. } = &actual.target {
            report_members(members, actualized, &declarations, headers, stable, diags);
        }
    }
}

impl UnmatchedActual {
    /// The stable identity the compact header inventory anchors on this declaration's own source
    /// range, within this declaration's own file.
    ///
    /// This is the ONLY identity this check uses, for every kind of declaration. Actualization's
    /// pairing — keyed by the same identities — is then the only answer to whether the declaration
    /// actualized anything. A package-qualified name/arity key was consulted as a second answer
    /// and had to go: it differs on a receiver's spelling exactly where actualization follows an
    /// `actual typealias`, so it reported pairs that had matched.
    fn stable(
        &self,
        headers: &crate::fir::StreamedHeaderModule,
    ) -> Option<crate::fir::DeclarationId> {
        headers
            .stubs
            .iter()
            .map(|stub| stub.id)
            .find(|&declaration| {
                headers
                    .declarations
                    .anchor(declaration)
                    .is_some_and(|anchor| {
                        anchor.source.raw() == self.file && anchor.range == self.anchor
                    })
            })
    }
}

impl UnmatchedActual {
    /// This declaration as the reference compiler's declaration renderer names it, or what about
    /// it could not be reached.
    ///
    /// Every resolved fact comes from the published contract, looked up by the declaration's own
    /// identity. The syntactic half — kind, name, parameter names, modifier words — was copied out
    /// of this declaration while its syntax was live and travels on [`Self::target`].
    fn render(
        &self,
        stable: crate::fir::DeclarationId,
        declarations: &ResolvedDeclarations<'_>,
    ) -> Result<String, &'static str> {
        match &self.target {
            Target::Function {
                name,
                context_parameters,
                parameters,
                type_parameters,
                modifiers,
            } => {
                let Some(Resolved::Function(signature)) = declarations.get(stable) else {
                    return Err("has no resolved signature");
                };
                render_function(
                    signature,
                    name,
                    context_parameters,
                    parameters,
                    type_parameters,
                    modifiers,
                )
            }
            Target::Property { name, modifiers } => match declarations.get(stable) {
                Some(Resolved::Property(signature)) => render_property(signature, name, modifiers),
                Some(Resolved::ExtensionProperty(signature)) => {
                    render_extension_property(signature, name, modifiers)
                }
                _ => Err("has no resolved property signature"),
            },
            Target::Classifier { name, shape, .. } => {
                let Some(signature) = declarations.classifier(stable) else {
                    return Err("has no resolved signature");
                };
                render_classifier(signature, name, shape)
            }
            Target::TypeAlias {
                qualified,
                name,
                visibility,
            } => render_type_alias(declarations, qualified, name, *visibility),
        }
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
    signature: &Signature,
    name: &str,
    context_parameters: &[String],
    parameters: &[String],
    formals: &[String],
    modifiers: &str,
) -> Result<String, &'static str> {
    let context = context_prefix(
        context_parameters,
        signature
            .params
            .get(..signature.context_count)
            .unwrap_or(&[]),
    )?;
    // A top-level declaration is always `final`.
    let rendered = render_callable(signature, "final", modifiers, formals, name, parameters)
        .ok_or("has a resolved signature that did not resolve to concrete types")?;
    Ok(format!("{context}{rendered}"))
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
    signature: &crate::resolve::SourcePropertySig,
    name: &str,
    modifiers: &str,
) -> Result<String, &'static str> {
    if signature.ty.mentions_pending() {
        return Err("has a type that did not resolve");
    }
    Ok(format!(
        "{}{} final actual {modifiers}{} {name}: {}",
        context_prefix(&signature.context_param_names, &signature.context_params)?,
        visibility(signature.visibility),
        if signature.is_var { "var" } else { "val" },
        signature.ty.source_name()
    ))
}

/// An extension property lives in its own receiver-keyed table, which is also the only place its
/// receiver survives resolution — and the receiver is what its rendering adds.
fn render_extension_property(
    signature: &crate::resolve::ExtPropSig,
    name: &str,
    modifiers: &str,
) -> Result<String, &'static str> {
    if signature.ty.mentions_pending() || signature.receiver.mentions_pending() {
        return Err("has a type that did not resolve");
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
    Ok(format!(
        "{} final actual {modifiers}{} {formals}{}.{name}: {}",
        visibility(signature.visibility),
        if signature.is_var { "var" } else { "val" },
        signature.receiver.source_name(),
        signature.ty.source_name()
    ))
}

/// `context(tally: Tally) `, or nothing where the declaration has no context parameters.
///
/// The reference compiler renders each parameter's NAME with its RESOLVED type, and prints the
/// whole group before the visibility slot — ahead of everything else the rendering says.
fn context_prefix(names: &[String], types: &[Ty]) -> Result<String, &'static str> {
    if types.is_empty() {
        return Ok(String::new());
    }
    if names.len() != types.len() {
        return Err("has context parameters whose names and resolved types do not line up");
    }
    if types.iter().any(|ty| ty.mentions_pending()) {
        return Err("has a context parameter whose type did not resolve");
    }
    Ok(format!(
        "context({}) ",
        names
            .iter()
            .zip(types)
            .map(|(name, ty)| format!("{name}: {}", ty.source_name()))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// A hoisted nested classifier's own name. The parser prefixes each hoisted declaration with its
/// owner's path so the two never collide in one arena; the declaration itself wrote only the last
/// segment, and that is what every diagnostic about it names.
fn simple_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
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
    /// Whether this singleton is its owner's `companion object`, which the reference compiler
    /// renders as a modifier word before `object`.
    is_companion: bool,
}

impl ClassifierShape {
    fn of(class: &crate::ast::ClassDecl, is_companion: bool) -> Self {
        Self {
            kind: class.kind,
            modality: class.modality,
            singleton: class.singleton,
            is_data: class.is_data,
            is_value: class.is_value,
            is_inner: class.inner_of.is_some(),
            is_fun_interface: class.is_fun_interface,
            is_companion,
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
        if self.is_companion {
            words.push("companion");
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
    signature: &crate::resolve::ClassSig,
    name: &str,
    shape: &ClassifierShape,
) -> Result<String, &'static str> {
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
    Ok(format!(
        "{} {} actual {} {name}{formals} : {supertypes}",
        visibility(signature.visibility),
        shape.modality(),
        shape.keyword()
    ))
}

fn render_type_alias(
    declarations: &ResolvedDeclarations<'_>,
    qualified: &str,
    name: &str,
    declared: Visibility,
) -> Result<String, &'static str> {
    let (formals, target) = declarations
        .alias_expansion(qualified)
        .ok_or("has no resolved expansion")?;
    if target.mentions_pending() {
        return Err("expands to a type that did not resolve");
    }
    let formals = if formals.is_empty() {
        String::new()
    } else {
        format!("<{}>", formals.join(", "))
    };
    Ok(format!(
        "{} final actual typealias {name}{formals} = {}",
        visibility(declared),
        target.source_name()
    ))
}
