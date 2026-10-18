//! Authoritative publication of stable source type-alias headers.

use super::*;

/// Publish every independent source alias in inventory order. A rejected alias remains a failed
/// stable declaration, but does not own or truncate any later alias's semantic publication.
pub(super) fn publish_all(
    headers: &crate::fir::StreamedHeaderModule,
    semantics: &ProductionSignatureSemantics<'_>,
    index: &mut crate::fir::ResolvedModuleIndex,
) -> Vec<crate::fir::DeclarationId> {
    headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == crate::fir::DeclarationKind::TypeAlias)
        .filter_map(|stub| {
            publish_type_alias(headers, semantics, index, stub)
                .err()
                .map(|()| stub.id)
        })
        .collect()
}

fn publish_type_alias(
    headers: &crate::fir::StreamedHeaderModule,
    semantics: &ProductionSignatureSemantics<'_>,
    index: &mut crate::fir::ResolvedModuleIndex,
    stub: &crate::fir::DeclarationStub,
) -> Result<(), ()> {
    let name = stub
        .lookup_name
        .and_then(|name| headers.lookup_names.get(name))
        .ok_or(())?;
    let anchor = headers
        .declarations
        .anchor(stub.id)
        .expect("a compact type-alias stub must retain its stable owner");
    let owner = match anchor.owner {
        Some(owner) => {
            let owner_header = index.declaration_header(owner).ok_or(())?;
            if owner_header
                .flags
                .has(crate::fir::DeclarationFlags::LOCAL_CLASS)
            {
                return Ok(());
            }
            let classifier = index.classifier_header(owner).ok_or(())?;
            let mut nested = Vec::new();
            let mut current = classifier.classifier;
            while let Some(parent) = current.nested_owner() {
                nested.push(current.nested_segment_ref());
                current = parent;
            }
            nested.push(current.segment_ref());
            nested.reverse();
            let mut semantic_owner = classifier.classifier.namespace();
            for segment in nested {
                semantic_owner = crate::types::type_name_child(semantic_owner, segment);
            }
            semantic_owner
        }
        None => {
            let file_scope = headers.scopes.file(stub.source).ok_or(())?;
            let mut package = crate::types::TypeName::ROOT;
            for segment in headers.scopes.path(file_scope.package) {
                package = crate::types::type_name_child(
                    package,
                    headers.lookup_names.get(*segment).ok_or(())?,
                );
            }
            package
        }
    };
    let identity = crate::types::type_name_child(owner, name);
    let crate::fir::HeaderDeclarationKind::TypeAlias { target: syntax, .. } = headers
        .syntax
        .declaration(stub.id)
        .map(|declaration| declaration.kind)
        .ok_or(())?
    else {
        return Err(());
    };
    let scope = crate::fir::SignatureScope {
        owner: stub.id,
        source: stub.source,
    };
    let expansion = semantics
        .resolve_explicit_header_type(scope, syntax)
        .map_err(|_| ())?;
    let target = crate::libraries::type_alias_target_classifier(expansion).ok_or(())?;
    index
        .publish_type_alias_header(
            stub.id,
            identity,
            target,
            expansion,
            crate::spelling::Spelled::default(),
        )
        .map_err(|_| ())
}
