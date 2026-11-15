//! Registers each multi-file module's top-level functions and properties under the JVM file facade
//! that will hold them, so the checker and common lowering name the same owner.

use crate::ast::{Decl, File};
use crate::frontend::FrontendSymbols;
use crate::jvm::names::file_class_name;
use crate::types::type_name;

pub fn prepare_module_symbols(files: &[File], stems: &[String], syms: &mut FrontendSymbols) {
    if files.len() <= 1 {
        return;
    }

    let mut fns: Vec<(u32, u32, Option<String>, String)> = Vec::new();
    let mut unemitted_fns: Vec<(u32, crate::ast::DeclId)> = Vec::new();
    let mut props: Vec<(u32, u32, String, String)> = Vec::new();
    let mut ext_props: Vec<(u32, u32, String)> = Vec::new();
    for (i, (file, stem)) in files.iter().zip(stems).enumerate() {
        let facade = file_class_name(stem, file.package.as_deref());
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    // This is the single owner of the emitted/unemitted decision. The checker
                    // consumes the declaration-keyed outcome recorded below; it must not repeat
                    // this predicate because signature/body support evolves with JVM lowering.
                    // The semantic handoff says whether a callable body exists; this JVM boundary
                    // owns only its representation as a facade static. Common IR lowering consumes
                    // the same answer, so registration cannot promise a body that lowering omits.
                    let emitted = syms.source_fn_has_callable_body(file, f);
                    if emitted {
                        fns.push((
                            i as u32,
                            d.0,
                            f.receiver.is_none().then(|| f.name.clone()),
                            facade.clone(),
                        ));
                    } else {
                        // Negative registration is as important as the facade map: it distinguishes
                        // a deliberately splice-only function from a checker-only pipeline where no
                        // JVM registration ran at all.
                        unemitted_fns.push((i as u32, d));
                    }
                }
                Decl::Property(p) if p.receiver.is_none() => {
                    props.push((i as u32, d.0, p.name.clone(), facade.clone()))
                }
                Decl::Property(_) => ext_props.push((i as u32, d.0, facade.clone())),
                _ => {}
            }
        }
    }

    for (file_index, decl_id, name, facade) in fns {
        let facade = type_name(&facade);
        syms.record_fn_facade(file_index, crate::ast::DeclId(decl_id), Some(facade));
        if let Some(name) = name {
            syms.fn_facades.insert(name, facade);
        }
    }
    for (file_index, declaration) in unemitted_fns {
        syms.record_fn_facade(file_index, declaration, None);
    }
    for (file, declaration, name, facade) in props {
        syms.prop_facades_by_decl
            .insert((file, declaration), type_name(&facade));
        if let Some(&(ty, is_var, is_const)) = syms.props.get(&name) {
            syms.prop_facades
                .insert(name, (type_name(&facade), ty, is_var, is_const));
        }
    }
    for (file_index, decl_id, facade) in ext_props {
        syms.ext_prop_facades_by_decl
            .insert((file_index, decl_id), type_name(&facade));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::frontend::{collect_signatures, parse_source_with_detected_features};

    #[test]
    fn prepare_module_symbols_records_cross_file_facades() {
        let mut diags = DiagSink::new();
        let files = vec![
            parse_source_with_detected_features(
                "package p\nfun helper(): String = \"OK\"\n\
                 inline operator fun String.unaryMinus(): String = this\n\
                 inline fun <reified T> spliceOnly(value: Any): T? = value as? T\n\
                 val answer: Int = 42",
                &mut diags,
            ),
            parse_source_with_detected_features(
                "package p\nfun box(): String = helper()",
                &mut diags,
            ),
        ];
        let stems = vec!["A".to_string(), "B".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);

        prepare_module_symbols(&files, &stems, &mut syms);

        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert_eq!(
            syms.fn_facades.get("helper").map(|facade| facade.render()),
            Some("p/AKt".to_string())
        );
        assert_eq!(
            syms.prop_facades
                .get("answer")
                .map(|(facade, _, _, _)| facade.render()),
            Some("p/AKt".to_string())
        );
        let extension = files[0]
            .decls
            .iter()
            .copied()
            .find(|&declaration| {
                matches!(
                    files[0].decl(declaration),
                    Decl::Fun(function)
                        if function.name == "unaryMinus" && function.receiver.is_some()
                )
            })
            .expect("source extension declaration");
        assert_eq!(
            syms.fn_facades_by_decl
                .get(&(0, extension.0))
                .map(|facade| facade.render()),
            Some("p/AKt".to_string())
        );
        let splice_only = files[0]
            .decls
            .iter()
            .copied()
            .find(|&declaration| {
                matches!(
                    files[0].decl(declaration),
                    Decl::Fun(function) if function.name == "spliceOnly"
                )
            })
            .expect("splice-only source declaration");
        assert!(
            syms.fn_facade_is_explicitly_unemitted(0, splice_only.0),
            "registration must preserve the negative outcome so checker-only absence is distinct"
        );
        assert!(!syms.fn_facades_by_decl.contains_key(&(0, splice_only.0)));
    }
}
