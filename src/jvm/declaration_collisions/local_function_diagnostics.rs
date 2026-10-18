//! Physical JVM method collisions between lifted local functions.
//!
//! Local-function numbering keys on the raw source name, so two local functions whose names
//! sanitize to one spelling (`my local` and `my-local`) both lift to `outer$my_local`. kotlinc
//! reports that platform declaration clash at each declaration.

use crate::diag::Span;
use crate::ir::{FunId, IrFile};

use super::UndeterminedDiagnosticType;

type ClashRows = Vec<(Span, String)>;

pub(super) fn report(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> Result<ClashRows, ClashRows> {
    let owners = ir
        .classes
        .iter()
        .enumerate()
        .flat_map(|(class, declaration)| {
            declaration
                .methods
                .iter()
                .map(move |&method| (method, class))
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();
    let mut functions = ir.local_function_spans.iter().collect::<Vec<_>>();
    functions.sort_by_key(|(function, span)| (span.lo, **function));
    let mut groups: Vec<((Option<usize>, &str, String), Vec<(FunId, Span)>)> = Vec::new();
    for (&function, &span) in functions {
        let key = (
            owners.get(&function).copied(),
            ir.functions[function as usize].name.as_str(),
            crate::jvm::ir_emit::function_descriptor(ir, override_results, function),
        );
        match groups.iter_mut().find(|(group, _)| *group == key) {
            Some((_, members)) => members.push((function, span)),
            None => groups.push((key, vec![(function, span)])),
        }
    }
    let mut clashes = Vec::new();
    for ((owner, name, descriptor), members) in groups {
        if members.len() < 2 {
            continue;
        }
        let owner = owner.map_or_else(
            || package_owner(ir).to_owned(),
            |class| ir.classes[class].fq_name.render().replace(['/', '$'], "."),
        );
        let listed = members
            .iter()
            .map(|&(function, _)| render(ir, function, &owner))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|kind| vec![(members[0].1, super::undetermined_type_message(kind))])?;
        let message = super::clash_message(name, &descriptor, &listed);
        clashes.extend(members.iter().map(|&(_, span)| (span, message.clone())));
    }
    clashes.sort_by_key(|(span, _)| span.lo);
    Ok(clashes)
}

/// The lifted method as kotlinc renders it: its physical parameters, spelled as their
/// local-variable names (a captured parameter `$y`, a captured local `v`).
fn render(ir: &IrFile, function: FunId, owner: &str) -> Result<String, UndeterminedDiagnosticType> {
    let declaration = &ir.functions[function as usize];
    let names = crate::jvm::parameter_names::function_locals(ir, function, &declaration.params)
        .and_then(|names| names.into_iter().collect::<Option<Vec<_>>>())
        .ok_or(UndeterminedDiagnosticType::InvalidContextParameterCount)?;
    super::render_physical_function(declaration, names.iter().map(String::as_str), owner)
}

fn package_owner(ir: &IrFile) -> &str {
    ir.package
        .as_deref()
        .filter(|package| !package.is_empty())
        .unwrap_or("root package")
}
