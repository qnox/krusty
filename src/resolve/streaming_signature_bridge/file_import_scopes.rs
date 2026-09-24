//! Per-file callable import scopes used while solving compact signatures.

use super::ProductionSignatureSemantics;
use crate::fir::{DiagnosticId, SourceFileId};
use crate::symbol_resolver::FunctionImportScope;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Imports resolved once per stable source-file identity. The header inventory and classpath are
/// immutable for the semantics' lifetime, so every signature in one file observes the same scope.
#[derive(Default)]
pub(super) struct FileImportScopes(RefCell<HashMap<SourceFileId, Rc<FunctionImportScope>>>);

impl ProductionSignatureSemantics<'_> {
    pub(super) fn function_import_scope(
        &self,
        source_id: SourceFileId,
    ) -> Result<Rc<FunctionImportScope>, DiagnosticId> {
        if let Some(scope) = self.file_import_scopes.0.borrow().get(&source_id) {
            return Ok(scope.clone());
        }
        let scope = Rc::new(self.resolve_function_import_scope(source_id)?);
        self.file_import_scopes
            .0
            .borrow_mut()
            .insert(source_id, scope.clone());
        Ok(scope)
    }

    fn resolve_function_import_scope(
        &self,
        source_id: SourceFileId,
    ) -> Result<FunctionImportScope, DiagnosticId> {
        let file = self
            .headers
            .scopes
            .file(source_id)
            .ok_or_else(Self::failure)?;
        let path = |range| {
            self.headers
                .scopes
                .path(range)
                .iter()
                .map(|segment| self.headers.lookup_names.get(*segment))
                .collect::<Option<Vec<_>>>()
                .map(|segments| segments.join("/"))
        };
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, source_id.raw());
        let symbols = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            self.table.libraries.as_ref() as &dyn crate::symbol_source::SymbolSource,
        ]);
        let mut explicit = HashMap::new();
        let mut stars = Vec::new();
        for import in self.headers.scopes.imports(file.imports) {
            let imported = path(import.path).ok_or_else(Self::failure)?;
            if import.wildcard {
                let owner = match super::super::qualifier_path(&imported, &symbols, None)
                    .map_err(|_| Self::failure())?
                {
                    super::super::ResolvedQualifier::Package(package) => package,
                    super::super::ResolvedQualifier::Classifier(classifier) => classifier,
                    super::super::ResolvedQualifier::Value => return Err(Self::failure()),
                };
                stars.push(owner);
                continue;
            }
            let (parent, declared_name) =
                imported.rsplit_once('/').unwrap_or(("", imported.as_str()));
            let owner = if parent.is_empty() {
                crate::symbol_source::SymbolNamespace::Package(crate::types::TypeName::ROOT)
            } else {
                match super::super::qualifier_path(parent, &symbols, None)
                    .map_err(|_| Self::failure())?
                {
                    super::super::ResolvedQualifier::Package(package) => {
                        crate::symbol_source::SymbolNamespace::Package(package)
                    }
                    super::super::ResolvedQualifier::Classifier(classifier) => {
                        crate::symbol_source::SymbolNamespace::Classifier(classifier)
                    }
                    super::super::ResolvedQualifier::Value => return Err(Self::failure()),
                }
            };
            let visible_name = import
                .alias
                .and_then(|alias| self.headers.lookup_names.get(alias))
                .unwrap_or(declared_name);
            explicit.insert(
                visible_name.to_owned(),
                crate::symbol_resolver::CallableImport::new(owner, declared_name.to_owned()),
            );
        }
        let own_package = path(file.package).ok_or_else(Self::failure)?;
        let kotlin_defaults = super::super::KOTLIN_DEFAULT_IMPORT_PACKAGES
            .iter()
            .map(|package| crate::types::type_name(&package.replace('.', "/")))
            .collect();
        let platform_defaults = self
            .table
            .libraries
            .platform_default_import_packages()
            .iter()
            .map(|package| crate::types::type_name(&package.replace('.', "/")))
            .collect();
        Ok(FunctionImportScope::new(
            explicit,
            [
                vec![crate::types::type_name(&own_package)],
                stars,
                kotlin_defaults,
                platform_defaults,
            ],
        ))
    }
}
