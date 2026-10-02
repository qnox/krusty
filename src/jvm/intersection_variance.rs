//! Classifier variance available while a backend encodes an intersection.
//!
//! Declaration approximation asks for each type parameter's variance. That fact lives on the
//! frozen module classifiers and on Kotlin builtins in the classpath, not on the semantic type.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::backend::BackendModuleFacts;
use crate::jvm::classpath::Classpath;
use crate::types::{TypeName, TypeVariance, VarianceScope};

struct Installed {
    module: HashMap<TypeName, Box<[TypeVariance]>>,
    classpath: Rc<Classpath>,
}

thread_local! {
    static INSTALLED: RefCell<Option<Installed>> = const { RefCell::new(None) };
}

fn lookup(classifier: TypeName, index: usize) -> TypeVariance {
    INSTALLED.with(|installed| {
        let installed = installed.borrow();
        let Some(installed) = installed.as_ref() else {
            return TypeVariance::Invariant;
        };
        if let Some(variances) = installed.module.get(&classifier) {
            return variances.get(index).copied().unwrap_or_default();
        }
        installed
            .classpath
            .builtin_class_variances_name(classifier)
            .and_then(|variances| variances.get(index).copied())
            .unwrap_or_default()
    })
}

pub(super) struct Guard {
    scope: Option<VarianceScope>,
}

impl Guard {
    pub(super) fn install(module: &BackendModuleFacts, classpath: Rc<Classpath>) -> Self {
        let variances = module
            .classifiers()
            .map(|(name, fact)| (name, fact.type_param_variances.clone()))
            .collect();
        INSTALLED.with(|installed| {
            *installed.borrow_mut() = Some(Installed {
                module: variances,
                classpath,
            });
        });
        Self {
            scope: Some(crate::types::enter_variance(lookup)),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.scope.take();
        INSTALLED.with(|installed| {
            *installed.borrow_mut() = None;
        });
    }
}
