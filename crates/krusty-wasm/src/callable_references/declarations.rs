//! Declaration identities shared by source references and independently checked KLIB references.

use krusty::backend::{DependencyExportSignatures, KlibDependencyCallable};
use krusty::fir::CallableId;
use krusty::fx_hash::HashMap;
use krusty::ir::{FunId, IrFile};
use krusty::types::{Ty, TypeName};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Declaration {
    Source {
        module: String,
        callable: CallableId,
    },
    Dependency(KlibDependencyCallable),
    Local {
        file: u32,
        function: FunId,
    },
    Constructor {
        classifier: TypeName,
        parameters: Box<[Ty]>,
        result: Ty,
    },
    Conversion {
        ordinal: u32,
        parameters: Box<[Ty]>,
        result: Ty,
        suspend: bool,
    },
    Invoke {
        arity: usize,
        suspend: bool,
    },
    Classifier {
        classifier: TypeName,
        operation: u8,
    },
}

#[derive(Default)]
pub(super) struct Declarations {
    identities: HashMap<Declaration, u32>,
    next: u32,
    files: u32,
}

impl Declarations {
    pub(super) fn intern(&mut self, declaration: Declaration) -> Result<u32, String> {
        self.aliases(&[declaration])
    }

    fn aliases(&mut self, aliases: &[Declaration]) -> Result<u32, String> {
        let mut existing = None;
        for alias in aliases {
            if let Some(&identity) = self.identities.get(alias) {
                if existing.is_some_and(|previous| previous != identity) {
                    return Err("conflicting callable-reference declaration identities".into());
                }
                existing = Some(identity);
            }
        }
        let identity = match existing {
            Some(identity) => identity,
            None => {
                let identity = self.next;
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or("too many callable-reference declarations")?;
                identity
            }
        };
        for alias in aliases {
            self.identities.insert(alias.clone(), identity);
        }
        Ok(identity)
    }

    pub(super) fn begin_file(
        &mut self,
        module: &str,
        ir: &IrFile,
        exports: &DependencyExportSignatures,
    ) -> Result<u32, String> {
        let file = self.files;
        self.files = self
            .files
            .checked_add(1)
            .ok_or("too many callable-reference files")?;
        for (&callable, &function) in &ir.checked_callable_functions {
            let mut aliases = vec![
                Declaration::Source {
                    module: module.to_owned(),
                    callable,
                },
                Declaration::Local { file, function },
            ];
            if let Some(dependency) = exports.callable(function) {
                aliases.push(Declaration::Dependency(dependency.clone()));
            }
            if let Some(dependency) = exports.interface_member(function) {
                aliases.push(Declaration::Dependency(dependency.clone()));
            }
            self.aliases(&aliases)?;
        }
        Ok(file)
    }
}
