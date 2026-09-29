//! Spread one classpath's `box()` calls across a lane of JVMs.
//!
//! A single runner accepts concurrent calls, and one JVM still leaves cores idle. Each classpath
//! grows a lane until `width` runners exist. An idle runner is reused. A different classpath takes
//! an idle runner's slot when the lane is already full, and still gets a runner when every one is
//! busy.

use std::collections::{HashMap, VecDeque};

struct Slot<R> {
    id: u64,
    inflight: usize,
    runner: R,
}

pub(crate) struct Claim<R> {
    pub(crate) id: u64,
    pub(crate) runner: R,
}

pub(crate) struct RunnerPool<R> {
    groups: HashMap<String, Vec<Slot<R>>>,
    order: VecDeque<String>,
    next_id: u64,
}

/// JVMs per classpath. `KRUSTY_BOX_RUNNER_POOL` overrides; otherwise one JVM per host CPU.
pub(crate) fn width_from_env() -> usize {
    std::env::var("KRUSTY_BOX_RUNNER_POOL")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |parallelism| parallelism.get())
        })
}

impl<R: Clone> RunnerPool<R> {
    pub(crate) fn new() -> Self {
        Self {
            groups: HashMap::new(),
            order: VecDeque::new(),
            next_id: 1,
        }
    }

    /// Reserve a runner for `cp`. `width` is the number of JVMs one classpath may keep, and the
    /// number of idle JVMs the process keeps in total. `create` runs only when a new JVM is
    /// required.
    pub(crate) fn acquire(
        &mut self,
        cp: &str,
        width: usize,
        alive: impl Fn(&R) -> bool,
        create: impl FnOnce() -> Option<R>,
    ) -> Option<Claim<R>> {
        let width = width.max(1);
        self.drop_dead(&alive);
        self.prune_idle(cp, width);
        if let Some(claim) = self.take_idle(cp, &alive) {
            self.touch(cp);
            return Some(claim);
        }
        let mine = self.groups.get(cp).map_or(0, Vec::len);
        let total = self.runner_count();
        if mine < width && (total < width || self.evict_idle_other(cp)) {
            return self.spawn(cp, create);
        }
        if mine > 0 {
            self.touch(cp);
            return Some(self.take_least_busy(cp, &alive));
        }
        self.spawn(cp, create)
    }

    pub(crate) fn release(&mut self, cp: &str, id: u64) {
        let Some(group) = self.groups.get_mut(cp) else {
            return;
        };
        if let Some(slot) = group.iter_mut().find(|slot| slot.id == id) {
            slot.inflight = slot.inflight.saturating_sub(1);
        }
    }

    /// Drop a runner that failed a call. Other in-flight callers of the same id release into a
    /// missing slot, which is a no-op.
    pub(crate) fn retire(&mut self, cp: &str, id: u64) {
        let Some(group) = self.groups.get_mut(cp) else {
            return;
        };
        group.retain(|slot| slot.id != id);
        if group.is_empty() {
            self.groups.remove(cp);
            self.order.retain(|key| key != cp);
        }
    }

    fn runner_count(&self) -> usize {
        self.groups.values().map(Vec::len).sum()
    }

    fn drop_dead(&mut self, alive: &impl Fn(&R) -> bool) {
        let mut empty = Vec::new();
        for (cp, group) in &mut self.groups {
            group.retain(|slot| slot.inflight > 0 || alive(&slot.runner));
            if group.is_empty() {
                empty.push(cp.clone());
            }
        }
        for cp in empty {
            self.groups.remove(&cp);
            self.order.retain(|key| key != &cp);
        }
    }

    fn prune_idle(&mut self, keep: &str, width: usize) {
        while self.runner_count() > width {
            if self.evict_idle_other(keep) {
                continue;
            }
            if !self.remove_idle(keep) {
                break;
            }
        }
    }

    fn evict_idle_other(&mut self, keep: &str) -> bool {
        let victim = self.order.iter().find_map(|cp| {
            if cp == keep {
                return None;
            }
            let idle = self
                .groups
                .get(cp)
                .is_some_and(|group| group.iter().any(|slot| slot.inflight == 0));
            idle.then(|| cp.clone())
        });
        let Some(victim) = victim else {
            return false;
        };
        self.remove_idle(&victim)
    }

    fn remove_idle(&mut self, cp: &str) -> bool {
        let Some(group) = self.groups.get_mut(cp) else {
            return false;
        };
        let Some(idx) = group.iter().position(|slot| slot.inflight == 0) else {
            return false;
        };
        group.remove(idx);
        if group.is_empty() {
            self.groups.remove(cp);
            self.order.retain(|key| key != cp);
        }
        true
    }

    fn take_idle(&mut self, cp: &str, alive: &impl Fn(&R) -> bool) -> Option<Claim<R>> {
        let group = self.groups.get_mut(cp)?;
        let idx = group
            .iter()
            .position(|slot| slot.inflight == 0 && alive(&slot.runner))?;
        group[idx].inflight += 1;
        Some(Claim {
            id: group[idx].id,
            runner: group[idx].runner.clone(),
        })
    }

    fn take_least_busy(&mut self, cp: &str, alive: &impl Fn(&R) -> bool) -> Claim<R> {
        let group = self
            .groups
            .get_mut(cp)
            .expect("a busy classpath still has a runner");
        let alive_idx = group
            .iter()
            .enumerate()
            .filter(|(_, slot)| alive(&slot.runner))
            .min_by_key(|(_, slot)| slot.inflight)
            .map(|(idx, _)| idx);
        let idx = alive_idx.unwrap_or_else(|| {
            group
                .iter()
                .enumerate()
                .min_by_key(|(_, slot)| slot.inflight)
                .map(|(idx, _)| idx)
                .expect("a busy classpath still has a runner")
        });
        group[idx].inflight += 1;
        Claim {
            id: group[idx].id,
            runner: group[idx].runner.clone(),
        }
    }

    fn spawn(&mut self, cp: &str, create: impl FnOnce() -> Option<R>) -> Option<Claim<R>> {
        let runner = create()?;
        let id = self.next_id;
        self.next_id += 1;
        self.groups.entry(cp.to_string()).or_default().push(Slot {
            id,
            inflight: 1,
            runner: runner.clone(),
        });
        self.touch(cp);
        Some(Claim { id, runner })
    }

    fn touch(&mut self, cp: &str) {
        self.order.retain(|key| key != cp);
        self.order.push_back(cp.to_string());
    }
}
