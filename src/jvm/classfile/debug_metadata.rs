//! kotlinc's `@DebugMetadata` on a coroutine's continuation: the source positions and spilled
//! locals of its suspension points, which the coroutine debugger reads.

use super::{u2, ClassWriter};
use crate::jvm::bytecode_passes::coroutines::DebugMetadata;

impl ClassWriter {
    /// Add the runtime-visible `DebugMetadata` annotation for a suspend continuation.
    pub(crate) fn set_debug_metadata(&mut self, metadata: &DebugMetadata) {
        let body = self.debug_metadata_annotation(metadata);
        self.runtime_annotations.push(body);
    }

    /// The `DebugMetadata` annotation of a class whose own method is its state machine, written
    /// after the class's annotations are queued: it leads them, as kotlinc writes it.
    pub(super) fn lead_with_debug_metadata(&mut self, metadata: &DebugMetadata) {
        let body = self.debug_metadata_annotation(metadata);
        self.runtime_annotations.insert(0, body);
    }

    fn debug_metadata_annotation(&mut self, metadata: &DebugMetadata) -> Vec<u8> {
        let anno_type = self
            .cp
            .utf8("Lkotlin/coroutines/jvm/internal/DebugMetadata;");
        let mut body = Vec::new();
        u2(&mut body, anno_type);
        u2(&mut body, 9); // element_value_pairs: f, l, nl, i, s, n, m, c, v
        let n_f = self.cp.utf8("f");
        u2(&mut body, n_f);
        self.ev_str(&mut body, &metadata.source_file);
        let n_l = self.cp.utf8("l");
        u2(&mut body, n_l);
        self.ev_int_array(&mut body, &metadata.line_numbers);
        let n_nl = self.cp.utf8("nl");
        u2(&mut body, n_nl);
        self.ev_int_array(&mut body, &metadata.next_line_numbers);
        let n_i = self.cp.utf8("i");
        u2(&mut body, n_i);
        self.ev_int_array(&mut body, &metadata.index_to_label);
        let n_s = self.cp.utf8("s");
        u2(&mut body, n_s);
        self.ev_str_array(&mut body, &metadata.spilled);
        let n_n = self.cp.utf8("n");
        u2(&mut body, n_n);
        self.ev_str_array(&mut body, &metadata.local_names);
        let n_m = self.cp.utf8("m");
        u2(&mut body, n_m);
        self.ev_str(&mut body, &metadata.method_name);
        let n_c = self.cp.utf8("c");
        u2(&mut body, n_c);
        self.ev_str(&mut body, &metadata.class_name);
        let n_v = self.cp.utf8("v");
        u2(&mut body, n_v);
        self.ev_int(&mut body, metadata.version);
        body
    }
}
