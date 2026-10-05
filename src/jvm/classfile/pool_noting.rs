//! What a span of class writing names in the constant pool.
//!
//! kotlinc's writer interns an entry when it first visits a holder that names it. Placing the
//! entries a span of krusty's writing names, found or added, where kotlinc visits that span (see
//! [`super::pool_layout`]) needs the indices the span's interns returned, not only the ones it
//! added.

impl super::ConstPool {
    /// Start recording the indices interns return.
    pub(super) fn start_noting(&mut self) {
        assert!(
            self.noted.is_none(),
            "constant-pool noting spans must not overlap"
        );
        self.noted = Some(Vec::new());
    }

    /// The indices interns returned since [`Self::start_noting`], each once, in order.
    pub(super) fn take_noted(&mut self) -> Vec<u16> {
        let mut noted = self
            .noted
            .take()
            .expect("constant-pool noting must be started before it is taken");
        let mut seen = std::collections::HashSet::new();
        noted.retain(|&index| seen.insert(index));
        noted
    }

    /// `index`, recorded while noting.
    pub(super) fn noting(&mut self, index: u16) -> u16 {
        if let Some(noted) = &mut self.noted {
            noted.push(index);
        }
        index
    }
}
