//! The in-memory overlay of declaration classes a compilation serves above its classpath entries.

use std::collections::HashMap;

use super::{Classpath, SymbolNamespace};
use crate::jvm::classreader::{parse_class, ClassInfo};
use crate::types::TypeName;

/// One lexical layer of in-memory declaration classes. A mixed-source compilation pushes its Java
/// header classes above any request-local dependency overlay and restores the previous entries when
/// the source-module provider is dropped. This keeps a reused classpath from leaking declarations
/// between compilation requests.
pub(in crate::jvm) struct StubOverlayGuard {
    classpath: std::rc::Rc<Classpath>,
    previous: Vec<(TypeName, Option<std::sync::Arc<ClassInfo>>)>,
}

impl Drop for StubOverlayGuard {
    fn drop(&mut self) {
        let affected = self
            .previous
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        {
            let mut overlay = self.classpath.stub_overlay.borrow_mut();
            for (name, previous) in self.previous.drain(..).rev() {
                match previous {
                    Some(class) => {
                        overlay.insert(name, class);
                    }
                    None => {
                        overlay.remove(&name);
                    }
                }
            }
        }
        self.classpath.invalidate_overlay_memos(affected);
    }
}

impl Classpath {
    /// Replace the in-memory class overlay and invalidate dependent lookups.
    pub fn set_stub_overlay(&self, classes: Vec<(String, Vec<u8>)>) {
        let mut map = HashMap::new();
        for (_, bytes) in classes {
            if let Ok(ci) = parse_class(&bytes) {
                map.entry(ci.this_class)
                    .or_insert_with(|| std::sync::Arc::new(ci));
            }
        }
        let affected = self
            .stub_overlay
            .borrow()
            .keys()
            .copied()
            .chain(map.keys().copied())
            .collect::<std::collections::HashSet<_>>();
        *self.stub_overlay.borrow_mut() = map;
        self.decode_overlay_inner_classes();
        self.invalidate_overlay_memos(affected);
    }

    /// Push source-module declaration classes without destroying an existing request-local
    /// overlay. The returned guard restores exactly the shadowed entries on drop.
    pub(in crate::jvm) fn push_stub_overlay(
        self: &std::rc::Rc<Self>,
        classes: Vec<(String, Vec<u8>)>,
    ) -> StubOverlayGuard {
        let mut parsed = HashMap::new();
        for (_, bytes) in classes {
            if let Ok(class) = parse_class(&bytes) {
                parsed
                    .entry(class.this_class)
                    .or_insert_with(|| std::sync::Arc::new(class));
            }
        }
        let affected = parsed.keys().copied().collect::<Vec<_>>();
        let mut previous = Vec::with_capacity(parsed.len());
        {
            let mut overlay = self.stub_overlay.borrow_mut();
            for (name, class) in parsed {
                previous.push((name, overlay.insert(name, class)));
            }
        }
        self.decode_overlay_inner_classes();
        self.invalidate_overlay_memos(affected);
        StubOverlayGuard {
            classpath: self.clone(),
            previous,
        }
    }

    pub(super) fn invalidate_overlay_memos(&self, classifiers: impl IntoIterator<Item = TypeName>) {
        let classifiers = classifiers.into_iter().collect::<Vec<_>>();
        if classifiers.is_empty() {
            return;
        }
        {
            let mut resolved = self.resolved_types.borrow_mut();
            for classifier in &classifiers {
                resolved.remove(classifier);
            }
        }
        let mut symbols = self.symbols_memo.borrow_mut();
        let mut packages = std::collections::HashSet::new();
        let mut classifier_namespaces = std::collections::HashSet::new();
        for classifier in classifiers {
            let mut outermost = classifier;
            classifier_namespaces.insert(classifier);
            while let Some(owner) = outermost.nested_owner() {
                classifier_namespaces.insert(owner);
                outermost = owner;
            }
            packages.insert(outermost.namespace());
        }
        for package in packages {
            symbols.remove(&SymbolNamespace::Package(package));
        }
        for classifier in classifier_namespaces {
            symbols.remove(&SymbolNamespace::Classifier(classifier));
        }
    }

    /// Remove all in-memory classes.
    pub fn clear_stub_overlay(&self) {
        if !self.stub_overlay.borrow().is_empty() {
            let affected = self
                .stub_overlay
                .borrow()
                .keys()
                .copied()
                .collect::<Vec<_>>();
            self.stub_overlay.borrow_mut().clear();
            self.invalidate_overlay_memos(affected);
        }
    }

    /// Decode every overlay inner class within its outer class, outer classes first so a nested
    /// inner class sees its outer's complete scope.
    fn decode_overlay_inner_classes(&self) {
        loop {
            let pending = self
                .stub_overlay
                .borrow()
                .iter()
                .filter_map(|(name, class)| Some((*name, class.metadata_outer_class()?)))
                .collect::<Vec<_>>();
            let ready = pending
                .iter()
                .filter(|(_, outer)| pending.iter().all(|(name, _)| name != outer))
                .copied()
                .collect::<Vec<_>>();
            if ready.is_empty() {
                return;
            }
            for (name, outer) in ready {
                let class = self.stub_overlay.borrow()[&name].clone();
                let decoded = match self.find_name(outer) {
                    Some(outer) => class.within_outer(&outer),
                    None => Ok(ClassInfo::clone(&class)),
                }
                .map(|class| class.without_inner_metadata());
                match decoded {
                    Ok(decoded) => {
                        self.stub_overlay
                            .borrow_mut()
                            .insert(name, std::sync::Arc::new(decoded));
                    }
                    Err(error) => {
                        self.record_class_load_error(name, error);
                        return;
                    }
                }
            }
        }
    }
}
