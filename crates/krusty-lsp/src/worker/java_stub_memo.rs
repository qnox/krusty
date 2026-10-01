//! One worker-local Java stub overlay.
//!
//! Sibling Java sources are stubbed on every analysis so Kotlin can resolve them. The sources and
//! the classpath contents are usually unchanged between keystrokes, and generating the stubs lexes
//! every Java file. An exact match reinstalls the previous class bytes. The key is the classpath
//! content snapshot, not the path list: a jar replaced at the same path resolves different types.

use krusty::jvm::classpath::Classpath;

/// The last Java stub overlay installed in this worker.
#[derive(Default)]
pub(super) struct JavaStubMemo {
    ready: bool,
    snapshot: Option<krusty::jvm::classpath::content_snapshot::ClasspathContentSnapshot>,
    java_sources: Vec<String>,
    stubs: Vec<(String, Vec<u8>)>,
}

#[cfg(test)]
thread_local! {
    static JAVA_STUB_GENERATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_java_stub_generations() {
    JAVA_STUB_GENERATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn java_stub_generations() -> u32 {
    JAVA_STUB_GENERATIONS.with(|count| count.get())
}

impl JavaStubMemo {
    /// Install stubs for `java_sources`. Returns whether the caller must clear the overlay.
    pub(super) fn install(&mut self, classpath: &Classpath, java_sources: &[String]) -> bool {
        if java_sources.is_empty() {
            return false;
        }
        let snapshot = classpath.content_snapshot();
        if self.ready
            && self.snapshot.as_ref() == Some(&snapshot)
            && self.java_sources == java_sources
        {
            classpath.set_stub_overlay(self.stubs.clone());
            return true;
        }
        let java = java_sources
            .iter()
            .map(|source| (String::new(), source.clone()))
            .collect::<Vec<_>>();
        let resolve = |candidate: &str| {
            classpath
                .find_name(krusty::types::type_name(candidate))
                .is_some()
        };
        let Some(stubs) = krusty::jvm::java_stub::stub_classes(
            &java,
            krusty::jvm::java_stub::StubMode::Lenient,
            &resolve,
        ) else {
            self.ready = false;
            return false;
        };
        #[cfg(test)]
        JAVA_STUB_GENERATIONS.with(|count| count.set(count.get().saturating_add(1)));
        self.snapshot = Some(snapshot);
        self.java_sources = java_sources.to_vec();
        self.stubs = stubs.clone();
        self.ready = true;
        classpath.set_stub_overlay(stubs);
        true
    }
}
