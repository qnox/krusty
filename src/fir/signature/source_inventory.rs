//! The parser's per-file declaration stream and the stable source order derived from it.

use super::{
    DeclarationFlags, DeclarationId, DeclarationIds, DeclarationKind, HashMap, ResolvedModuleIndex,
    SourceFileId,
};

impl ResolvedModuleIndex {
    pub(crate) fn source_inventory(&self, source: SourceFileId) -> &[DeclarationId] {
        self.source_inventory
            .get(&source)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn source_order(&self, declaration: DeclarationId) -> Option<u32> {
        let order = self.source_orders.get(declaration).copied();
        if order.is_none() {
            crate::trace_compiler!(
                "fir",
                "missing source order for {declaration:?}: anchor={:?}, header={:?}",
                self.declaration_anchor(declaration),
                self.declaration_header(declaration),
            );
        }
        order
    }

    pub(in crate::fir) fn publish_source_inventory(
        &mut self,
        inventory: &[DeclarationId],
        declarations: &DeclarationIds,
    ) {
        assert!(
            self.source_inventory.is_empty(),
            "source declaration inventory may be published only once"
        );
        let mut grouped = HashMap::<SourceFileId, Vec<DeclarationId>>::default();
        for &declaration in inventory {
            let source = declarations
                .anchor(declaration)
                .expect("inventoried declaration must retain its stable source")
                .source;
            grouped.entry(source).or_default().push(declaration);
        }
        // `inventory` is the parser's semantic declaration stream. Preserve that order exactly:
        // ranges are diagnostic payload, and released ordinary accessor bodies may legitimately
        // fall back to their owner's span on a later bounded parse. Sorting by those spans turns a
        // getter/setter pair into a body locator and can swap their stable identities.
        for source_declarations in grouped.values() {
            for (order, declaration) in source_declarations.iter().copied().enumerate() {
                self.source_orders.insert(
                    declaration,
                    u32::try_from(order).expect("too many source declarations"),
                );
            }
        }
        self.publish_generated_source_orders(declarations);
        self.source_inventory = grouped
            .into_iter()
            .map(|(source, declarations)| (source, declarations.into_boxed_slice()))
            .collect();
    }

    /// A retained inline/default body can discover compiler-generated capture storage after the
    /// parser declaration stream has been inventoried. Such a field has no independent source
    /// position: its exact ordering anchor is its owning declaration. Do not put it into the
    /// active-source inventory (there is no Pass-2 syntax to bind); publish only that structural
    /// ordering fact. Restrict this to generated declarations so a missing written declaration
    /// remains an invariant failure instead of silently inheriting an owner's position.
    ///
    /// A plugin callable a classifier declares (a hand-written companion's `serializer()`) is the
    /// one exception to the owner anchor: it follows every written member of that classifier, so
    /// it orders just past the classifier's last source declaration.
    fn publish_generated_source_orders(&mut self, declarations: &DeclarationIds) {
        let subtree_ends = subtree_ends(&self.source_orders, declarations);
        loop {
            let mut added = false;
            for raw in 0..declarations.len() {
                let declaration = DeclarationId::from_raw(
                    u32::try_from(raw).expect("too many stable declarations for a packed id"),
                );
                if self.source_orders.contains_key(declaration)
                    || !self.declaration_header(declaration).is_some_and(|header| {
                        header.flags.has(DeclarationFlags::COMPILER_GENERATED)
                    })
                {
                    continue;
                }
                let Some(owner) = declarations
                    .anchor(declaration)
                    .and_then(|anchor| anchor.owner)
                else {
                    continue;
                };
                let after_members = self
                    .is_classifier_plugin_callable(declaration, owner, declarations)
                    .then(|| subtree_ends.get(&owner).map(|end| end + 1))
                    .flatten();
                let Some(order) = after_members.or_else(|| self.source_orders.get(owner).copied())
                else {
                    continue;
                };
                self.source_orders.insert(declaration, order);
                added = true;
            }
            if !added {
                break;
            }
        }
    }

    /// Whether `declaration` is a function a frontend plugin contributed to the classifier `owner`.
    fn is_classifier_plugin_callable(
        &self,
        declaration: DeclarationId,
        owner: DeclarationId,
        declarations: &DeclarationIds,
    ) -> bool {
        declarations
            .anchor(owner)
            .is_some_and(|owner| owner.kind == DeclarationKind::Classifier)
            && self
                .callable_for_declaration(declaration)
                .is_some_and(|callable| {
                    self.callable_behavior(callable.id)
                        .plugin_expression
                        .is_some()
                })
    }
}

/// The last source order inside each owner's declaration subtree.
fn subtree_ends(
    source_orders: &super::super::DeclarationTable<u32>,
    declarations: &DeclarationIds,
) -> HashMap<DeclarationId, u32> {
    let mut ends = HashMap::<DeclarationId, u32>::default();
    for (declaration, &order) in source_orders.iter() {
        let mut owner = declarations
            .anchor(declaration)
            .and_then(|anchor| anchor.owner);
        while let Some(current) = owner {
            let end = ends.entry(current).or_insert(order);
            *end = (*end).max(order);
            owner = declarations.anchor(current).and_then(|anchor| anchor.owner);
        }
    }
    ends
}
