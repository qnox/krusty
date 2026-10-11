//! The source spelling of a declared type: which `typealias` each node named and which type-use
//! annotations each occurrence carries, walked beside the resolved `Ty` (see
//! [`crate::spelling::Spelled`]).

use super::{type_argument_of_ref, ClassNames, HashMap, TParams, Ty, TypeName, TypeRef};
use crate::diag::DiagSink;
use crate::spelling::Spelled;

/// The SOURCE SPELLING of a declared type, walked in parallel with [`ty_of_ref_with`] over the same
/// `TypeRef` — the sidecar `@Metadata` needs to write `Type.abbreviated_type` (see
/// [`crate::spelling::Spelled`]).
///
/// This is a SEPARATE walk rather than an extra return value from `ty_of_ref_with` deliberately:
/// that function is on the hot path of every signature collection and every checker query, and the
/// spelling is wanted only where a declaration is published to metadata. Diagnostics are suppressed
/// here for the same reason — `ty_of_ref_with` has already reported anything wrong with this
/// `TypeRef`, and reporting twice would double every type error in a declared position.
pub(crate) fn spelling_of_ref(
    r: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    expansions: &HashMap<TypeName, (Spelled, Vec<String>, Ty)>,
    spellings: crate::spelling::SourceSpellings<'_>,
) -> Spelled {
    let mut sink = DiagSink::new();
    spelling_of_ref_with(
        r,
        classes,
        tparams,
        expansions,
        spellings,
        &mut |argument| type_argument_of_ref(argument, classes, tparams, &mut sink),
    )
}

/// [`spelling_of_ref`] with declaration-scoped semantic argument resolution supplied by the
/// caller. Compact Pass-1 headers use this form so alias arguments are bound by the same lexical
/// resolver as the declaration signature instead of being looked up again in a file-global map.
pub(crate) fn spelling_of_ref_with(
    r: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    expansions: &HashMap<TypeName, (Spelled, Vec<String>, Ty)>,
    spellings: crate::spelling::SourceSpellings<'_>,
    resolve_argument: &mut dyn FnMut(&TypeRef) -> Ty,
) -> Spelled {
    // A source use stays written until name resolution selects its alias. A spelling parked in
    // `File::alias_spellings` comes only from a later declaration-template rewrite and preserves
    // that template's original abbreviation. An import path names a declaration rather than using
    // the type, and gets no abbreviation.
    let spelled = spellings.aliases.get(&r.span).unwrap_or(r);
    let annotations = spellings.annotations.at(r.span.lo).to_vec();
    // A type parameter shadows any same-named alias, and is never itself one.
    if tparams.contains(&spelled.name) {
        return Spelled {
            definitely_non_null: spelled.definitely_non_null(),
            annotations,
            ..Spelled::default()
        };
    }
    // Arrow syntax (`(A) -> B`) spells a function type structurally, so the NODE itself names no
    // alias — but its components can (`(Cargo) -> Cargo`). The metadata arguments of a function
    // type are synthesized as `params… + ret`, so the component spellings are laid out in that
    // order for the encoder to consume positionally; a named parameter's spelling carries its name.
    // A SUSPEND function type's tail is the CPS
    // `Continuation`/`Any?` pair instead of the return, so its return spelling has no slot.
    //
    // This is tested on the SPELLED node, not the resolved one: an alias right-hand side and a
    // written function type use arrow syntax, while a use of `typealias Handler<T> = (T) -> String`
    // still spells `Handler` and takes the alias path below.
    if !spelled.fun_params.is_empty() || spelled.name == "<fun>" {
        let mut args: Vec<Spelled> = spelled
            .fun_params
            .iter()
            .map(|parameter| Spelled {
                parameter_name: spellings
                    .annotations
                    .parameter_name_at(parameter.span.lo)
                    .map(Into::into),
                ..spelling_of_ref_with(
                    parameter,
                    classes,
                    tparams,
                    expansions,
                    spellings,
                    resolve_argument,
                )
            })
            .collect();
        if !spelled.fun_suspend() {
            args.push(
                spelled
                    .arg
                    .as_deref()
                    .map(|ret| {
                        spelling_of_ref_with(
                            ret,
                            classes,
                            tparams,
                            expansions,
                            spellings,
                            resolve_argument,
                        )
                    })
                    .unwrap_or_default(),
            );
        }
        return Spelled {
            definitely_non_null: spelled.definitely_non_null(),
            alias: None,
            alias_args: Vec::new(),
            args,
            annotations,
            ..Spelled::default()
        };
    }
    let alias = (!r.is_import())
        .then(|| classes.alias_identity(&spelled.name))
        .flatten();
    // Argument spellings come from the SPELLED node: at an aliased reference these are the
    // as-written arguments, whose arity may differ from the expansion's.
    let argument_spellings: Vec<Spelled> = spelled
        .targs
        .iter()
        .map(|argument| {
            spelling_of_ref_with(
                argument,
                classes,
                tparams,
                expansions,
                spellings,
                resolve_argument,
            )
        })
        .collect();
    let Some(alias) = alias else {
        return Spelled {
            definitely_non_null: spelled.definitely_non_null(),
            alias: None,
            alias_args: Vec::new(),
            // Without an alias at this node the expanded type's arguments ARE the spelled ones,
            // position for position.
            args: argument_spellings,
            annotations,
            ..Spelled::default()
        };
    };
    // At an aliased node the two argument lists diverge: the abbreviated `Type` takes the
    // AS-SPELLED arguments (`Boxed<Int>` -> one), while the expanded type takes the alias's
    // right-hand side applied to them (`PBox<Int, Int>` -> two). Recover each spelled argument's
    // `Ty` through the ordinary resolution path, discarding diagnostics as described above.
    let alias_args: Vec<(Ty, Spelled)> = spelled
        .targs
        .iter()
        .zip(argument_spellings)
        .map(|(argument, spelling)| (resolve_argument(argument), spelling))
        .collect();
    let template = expansions
        .get(&alias)
        .map(|(rhs, formals, expansion)| (rhs, formals.as_slice(), *expansion))
        .or_else(|| {
            classes.alias_expansion(&spelled.name).map(|classpath| {
                (
                    &classpath.expansion_spelling,
                    classpath.formals.as_slice(),
                    classpath.expansion,
                )
            })
        });
    // The right-hand side's root annotations, including those IT inherited from an alias it names.
    let expansion_annotations = template
        .map(|(rhs, _, _)| {
            let inherited = rhs.expansion_annotations.iter();
            inherited.chain(&rhs.annotations).cloned().collect()
        })
        .unwrap_or_default();
    let expansion_args = expansion_arg_spellings(template, &alias_args);
    Spelled {
        definitely_non_null: spelled.definitely_non_null(),
        alias: Some(alias),
        alias_args,
        // The expansion's argument spellings come from TWO places. A right-hand side that spells an
        // alias in a fixed position supplies it directly (`typealias CargoBox = PBox<Cargo, Cargo>`
        // abbreviates both expanded arguments as `Cargo`). A position holding one of the alias's
        // own PARAMETERS instead takes the spelling THIS use site wrote there (`typealias Boxed<T>
        // = PBox<T, T>` spells no alias itself, yet `Boxed<Cargo>` abbreviates both).
        //
        // A SOURCE alias's template comes from the module's own map; a CLASSPATH alias's comes from
        // its recorded expansion, whose right-hand-side spellings the metadata decoder recovers
        // from the dependency's `Type.abbreviated_type`.
        args: expansion_args,
        annotations,
        expansion_annotations,
        parameter_name: None,
    }
}

/// Place a use site's argument spellings into an alias expansion's PARAMETER positions, keeping the
/// right-hand side's own spelling everywhere else. See the call site for why both sources exist.
pub(super) fn expansion_arg_spellings(
    template: Option<(&Spelled, &[String], Ty)>,
    use_site: &[(Ty, Spelled)],
) -> Vec<Spelled> {
    let Some((rhs, formals, expansion)) = template else {
        return Vec::new();
    };
    let bindings = formals
        .iter()
        .cloned()
        .zip(use_site.iter().map(|(ty, _)| *ty))
        .collect::<crate::symbol_resolver::GSigBinds>();
    let applied = crate::symbol_resolver::ty_subst_keep_unbound(expansion, &bindings);
    spelled_arguments(expansion)
        .into_iter()
        .zip(spelled_arguments(applied))
        .enumerate()
        .map(|(index, (template, applied))| {
            substitute_expansion_spelling(
                rhs.arg(index),
                template,
                applied,
                formals,
                use_site,
                &bindings,
            )
        })
        .collect()
}

/// The spelling of an alias parameter's occurrence once the use site's argument replaced it. The
/// argument keeps its own spelling (its alias, its annotations), and the occurrence keeps what the
/// right-hand side wrote there: its annotations, which the expanded type records ahead of the
/// argument's (`typealias Tagged<T> = (@Kept T) -> Unit` used as `Tagged<@Mark Item>` records
/// `@Kept @Mark Item`), and a function-type parameter's name.
fn substituted_parameter_spelling(occurrence: &Spelled, argument: Spelled) -> Spelled {
    let inherited = occurrence
        .expansion_annotations
        .iter()
        .chain(&occurrence.annotations)
        .cloned();
    let mut spelling = argument;
    // An aliased argument's own annotations belong to its abbreviation as well; the inherited ones
    // go only on the expanded type, as an alias's right-hand-side annotations do.
    let expanded = if spelling.alias.is_some() {
        &mut spelling.expansion_annotations
    } else {
        &mut spelling.annotations
    };
    *expanded = inherited.chain(std::mem::take(expanded)).collect();
    spelling.definitely_non_null |= occurrence.definitely_non_null;
    if occurrence.parameter_name.is_some() {
        spelling.parameter_name = occurrence.parameter_name.clone();
    }
    spelling
}

/// The types a spelling's [`Spelled::args`] describe, position for position: a classifier's type
/// arguments, or a function type's parameters followed by its return, which a suspend function
/// type's spelling has no slot for (see [`spelling_of_ref_with`]).
fn spelled_arguments(ty: Ty) -> Vec<Ty> {
    match ty.non_null() {
        Ty::Fun(signature) => {
            let mut arguments = signature.params.to_vec();
            if !signature.suspend {
                arguments.push(signature.ret);
            }
            arguments
        }
        _ => ty.type_args().to_vec(),
    }
}

/// Apply alias use-site arguments to the spelling tree of its expanded right-hand side. Semantic
/// expansion already substitutes recursively; the spelling sidecar must do the same or a nested
/// abbreviation (`Outer<T> = Box<Inner<T>>`) retains the alias declaration's `T` and later tries to
/// emit it in an unrelated declaration's metadata table.
fn substitute_expansion_spelling(
    spelling: &Spelled,
    template: Ty,
    applied: Ty,
    formals: &[String],
    use_site: &[(Ty, Spelled)],
    bindings: &crate::symbol_resolver::GSigBinds,
) -> Spelled {
    let template_inner = template.projection_inner().unwrap_or(template);
    if let Ty::TyParam(name, _) = template_inner {
        if let Some(index) = formals.iter().position(|formal| formal == name) {
            let argument = use_site
                .get(index)
                .map(|(_, spelling)| spelling.clone())
                .unwrap_or_default();
            return substituted_parameter_spelling(spelling, argument);
        }
    }

    let alias_args = spelling
        .alias_args
        .iter()
        .map(|(argument, argument_spelling)| {
            let applied_argument =
                crate::symbol_resolver::ty_subst_keep_unbound(*argument, bindings);
            (
                applied_argument,
                substitute_expansion_spelling(
                    argument_spelling,
                    *argument,
                    applied_argument,
                    formals,
                    use_site,
                    bindings,
                ),
            )
        })
        .collect();
    let args = spelled_arguments(template)
        .into_iter()
        .zip(spelled_arguments(applied))
        .enumerate()
        .map(|(index, (template, applied))| {
            substitute_expansion_spelling(
                spelling.arg(index),
                template,
                applied,
                formals,
                use_site,
                bindings,
            )
        })
        .collect();
    Spelled {
        definitely_non_null: spelling.definitely_non_null,
        alias: spelling.alias,
        alias_args,
        args,
        annotations: spelling.annotations.clone(),
        expansion_annotations: spelling.expansion_annotations.clone(),
        parameter_name: spelling.parameter_name.clone(),
    }
}
