//! Physical exception classes keyed by their authenticated declaration identity.

use krusty::backend::BackendClassifierSource;
use krusty::fx_hash::{HashMap, HashSet};
use krusty::libraries::{RuntimeExceptionKind, RuntimeExceptionLayout};
use krusty::types::TypeName;

use super::{CompositeType, Module, SubType, ValType};

#[cfg(test)]
mod tests;

pub(super) struct RuntimeClasses {
    base: u32,
    table: u32,
    string: ValType,
    declared: HashMap<TypeName, (RuntimeExceptionLayout, u32)>,
    /// The classes declared since [`Self::take_declared`] last ran, with their structs.
    fresh: Vec<(TypeName, u32)>,
}

impl RuntimeClasses {
    pub(super) fn new(base: u32, table: u32, string: ValType) -> Self {
        Self {
            base,
            table,
            string,
            declared: HashMap::default(),
            fresh: Vec::new(),
        }
    }

    pub(super) fn ensure(
        &mut self,
        module: &mut Module,
        source: &dyn BackendClassifierSource,
        identity: TypeName,
    ) -> Result<u32, String> {
        let mut chain = Vec::new();
        let mut seen = HashSet::default();
        let mut next = identity;
        loop {
            if !seen.insert(next) {
                return Err(format!("a cyclic runtime exception superclass at {next}"));
            }
            let fact = source
                .classifier(next)
                .ok_or_else(|| format!("no frozen runtime exception declaration for {next}"))?;
            let layout = fact
                .runtime_exception
                .ok_or_else(|| format!("no authenticated runtime exception layout for {next}"))?;
            if layout.kind.classifier() != next {
                return Err(format!(
                    "a runtime exception role disagrees with declaration {next}"
                ));
            }
            if let Some((previous, _)) = self.declared.get(&next) {
                if *previous != layout {
                    return Err(format!(
                        "runtime exception declaration {next} changed during emission"
                    ));
                }
            }
            chain.push((next, layout));
            match (layout.kind, layout.parent) {
                (RuntimeExceptionKind::Throwable, None) => break,
                (RuntimeExceptionKind::Throwable, Some(_)) | (_, None) => {
                    return Err(format!("an incomplete runtime exception parent for {next}"))
                }
                (_, Some(parent)) => next = parent,
            }
        }
        let mut parent = self.base;
        for (name, layout) in chain.into_iter().rev() {
            let structure = match self.declared.get(&name) {
                Some((_, structure)) => *structure,
                None if layout.kind == RuntimeExceptionKind::Throwable => {
                    self.fresh.push((name, self.base));
                    self.base
                }
                None => {
                    let structure = module.subtype(SubType {
                        composite: CompositeType::Struct(super::ExceptionTransport::fields(
                            self.table,
                            self.string,
                        )),
                        supertype: Some(parent),
                        open: true,
                    });
                    self.fresh.push((name, structure));
                    structure
                }
            };
            self.declared.insert(name, (layout, structure));
            parent = structure;
        }
        Ok(parent)
    }

    pub(super) fn take_declared(&mut self) -> Vec<(TypeName, u32)> {
        std::mem::take(&mut self.fresh)
    }
}
