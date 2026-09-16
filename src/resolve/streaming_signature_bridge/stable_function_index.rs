//! Reverse lookup from stable declarations to source callable signatures.

use std::cell::{Ref, RefCell};
use std::collections::HashMap;

use crate::fir::DeclarationId;
use crate::types::{Ty, TypeName};

use super::{member_extension_receiver, ProductionSignatureSemantics, Signature, SymbolTable};

/// Where in the symbol table a stable declaration's signature lives.
///
/// Positions rather than references let the index remain cached while the table is borrowed again
/// for the lookup itself.
#[derive(Clone)]
enum StableFunctionSite {
    Fun(String, usize),
    ExtFun(String, Ty, usize),
    Method(TypeName, String, usize),
    MemberExt(TypeName, String, usize),
}

#[derive(Default)]
pub(super) struct StableFunctionIndex {
    sites: RefCell<Option<HashMap<DeclarationId, StableFunctionSite>>>,
}

impl StableFunctionIndex {
    /// Build the reverse index once in the same order as the former linear scan.
    fn sites<'a>(
        &'a self,
        table: &SymbolTable,
    ) -> Ref<'a, HashMap<DeclarationId, StableFunctionSite>> {
        if self.sites.borrow().is_none() {
            let mut sites = HashMap::new();
            let mut record = |declaration, site| {
                sites.entry(declaration).or_insert(site);
            };
            for (name, signatures) in &table.funs {
                for (index, signature) in signatures.iter().enumerate() {
                    if let Some(declaration) = signature.stable_declaration {
                        record(declaration, StableFunctionSite::Fun(name.clone(), index));
                    }
                }
            }
            for (name, by_receiver) in &table.ext_funs {
                for (receiver, signatures) in by_receiver {
                    for (index, signature) in signatures.iter().enumerate() {
                        if let Some(declaration) = signature.stable_declaration {
                            record(
                                declaration,
                                StableFunctionSite::ExtFun(name.clone(), *receiver, index),
                            );
                        }
                    }
                }
            }
            for (class_name, class) in &table.classes {
                for (name, signatures) in &class.methods {
                    for (index, signature) in signatures.iter().enumerate() {
                        if let Some(declaration) = signature.stable_declaration {
                            record(
                                declaration,
                                StableFunctionSite::Method(*class_name, name.clone(), index),
                            );
                        }
                    }
                }
            }
            for (class_name, class) in &table.classes {
                for (name, functions) in &class.member_ext_funs {
                    for (index, function) in functions.iter().enumerate() {
                        if let Some(declaration) = function.signature().stable_declaration {
                            record(
                                declaration,
                                StableFunctionSite::MemberExt(*class_name, name.clone(), index),
                            );
                        }
                    }
                }
            }
            *self.sites.borrow_mut() = Some(sites);
        }
        Ref::map(self.sites.borrow(), |sites| {
            sites.as_ref().expect("the index was just built")
        })
    }

    fn lookup<'a>(
        &self,
        table: &'a SymbolTable,
        declaration: DeclarationId,
    ) -> Option<(&'a Signature, Option<Ty>)> {
        let site = self.sites(table).get(&declaration).cloned()?;
        match site {
            StableFunctionSite::Fun(name, index) => {
                let signature = table.funs.get(&name)?.get(index)?;
                Some((signature, signature.source_receiver))
            }
            StableFunctionSite::ExtFun(name, receiver, index) => {
                let signature = table.ext_funs.get(&name)?.get(&receiver)?.get(index)?;
                Some((signature, signature.source_receiver))
            }
            StableFunctionSite::Method(class, name, index) => {
                let signature = table.classes.get(&class)?.methods.get(&name)?.get(index)?;
                Some((signature, signature.source_receiver))
            }
            StableFunctionSite::MemberExt(class, name, index) => {
                let function = table
                    .classes
                    .get(&class)?
                    .member_ext_funs
                    .get(&name)?
                    .get(index)?;
                Some((
                    function.signature(),
                    Some(member_extension_receiver(function)),
                ))
            }
        }
    }
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn callable_signature(&self, declaration: DeclarationId) -> Option<&Signature> {
        // The index contains exactly the tables the former scan searched. Its insertion order keeps
        // the old first match, and an index miss is therefore conclusive.
        self.stable_functions
            .lookup(self.table, declaration)
            .map(|(signature, _)| signature)
    }
}
