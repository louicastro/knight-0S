#![cfg_attr(not(test), no_std)]
//! KnightOS Preemptive SMP Scheduler Core.
//!
//! Combines a strict-priority Realtime band (`SchedClass::Realtime`) with an
//! EEVDF-inspired virtual-deadline fair band (`Interactive`, `Normal`, `Idle`).

extern crate alloc;

use alloc::collections::VecDeque;

pub type ThreadId = u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SchedClass {
    Realtime(u8), // 0..=31 (higher = higher priority)
    Interactive,
    Normal,
    Idle,
}

impl SchedClass {
    pub const fn weight(self) -> u64 {
        match self {
            SchedClass::Realtime(_) => 4096,
            SchedClass::Interactive => 2048,
            SchedClass::Normal => 1024,
            SchedClass::Idle => 128,
        }
    }

    pub const fn slice_ns(self) -> u64 {
        match self {
            SchedClass::Realtime(_) => 2_000_000,
            SchedClass::Interactive => 4_000_000,
            SchedClass::Normal => 10_000_000,
            SchedClass::Idle => 20_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SchedEntity {
    pub tid: ThreadId,
    pub class: SchedClass,
    pub vruntime: u64,
    pub vdeadline: u64,
}

impl SchedEntity {
    pub fn new(tid: ThreadId, class: SchedClass, min_vruntime: u64) -> Self {
        let vslice = (class.slice_ns() * 1024) / class.weight();
        Self {
            tid,
            class,
            vruntime: min_vruntime,
            vdeadline: min_vruntime.saturating_add(vslice),
        }
    }

    pub fn charge_tick(&mut self, delta_ns: u64) {
        let delta_v = (delta_ns * 1024) / self.class.weight().max(1);
        self.vruntime = self.vruntime.saturating_add(delta_v);
        if self.vruntime >= self.vdeadline {
            let vslice = (self.class.slice_ns() * 1024) / self.class.weight();
            self.vdeadline = self.vruntime.saturating_add(vslice);
        }
    }
}

/// Lock-external runnable queue selecting RT tasks first, then earliest virtual deadline.
pub struct RunQueue {
    ready: VecDeque<SchedEntity>,
    min_vruntime: u64,
}

impl RunQueue {
    pub const fn new() -> Self {
        Self {
            ready: VecDeque::new(),
            min_vruntime: 0,
        }
    }

    pub fn min_vruntime(&self) -> u64 {
        self.min_vruntime
    }

    pub fn enqueue(&mut self, mut ent: SchedEntity) {
        if ent.vruntime < self.min_vruntime {
            ent.vruntime = self.min_vruntime;
        }
        self.ready.push_back(ent);
    }

    pub fn len(&self) -> usize {
        self.ready.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    pub fn pick_next(&mut self) -> Option<SchedEntity> {
        if self.ready.is_empty() {
            return None;
        }

        // 1. Check strict Realtime priority band first.
        let mut best_rt_idx: Option<usize> = None;
        let mut best_rt_prio: u8 = 0;
        for (idx, ent) in self.ready.iter().enumerate() {
            if let SchedClass::Realtime(p) = ent.class
                && (best_rt_idx.is_none() || p > best_rt_prio)
            {
                best_rt_idx = Some(idx);
                best_rt_prio = p;
            }
        }
        if let Some(idx) = best_rt_idx {
            return self.ready.remove(idx);
        }

        // 2. Pick earliest virtual deadline (EEVDF-style) among non-RT runnable entities.
        let mut best_idx = 0usize;
        let mut best_vd = self.ready[0].vdeadline;
        for (idx, ent) in self.ready.iter().enumerate().skip(1) {
            if ent.vdeadline < best_vd {
                best_vd = ent.vdeadline;
                best_idx = idx;
            }
        }
        let picked = self.ready.remove(best_idx)?;
        if picked.vruntime > self.min_vruntime {
            self.min_vruntime = picked.vruntime;
        }
        Some(picked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_prefers_rt_then_interactive_vdeadline() {
        let mut rq = RunQueue::new();
        rq.enqueue(SchedEntity::new(1, SchedClass::Normal, 0));
        rq.enqueue(SchedEntity::new(2, SchedClass::Interactive, 0));
        rq.enqueue(SchedEntity::new(3, SchedClass::Realtime(10), 0));

        assert_eq!(rq.pick_next().unwrap().tid, 3);
        assert_eq!(rq.pick_next().unwrap().tid, 2);
        assert_eq!(rq.pick_next().unwrap().tid, 1);
        assert!(rq.pick_next().is_none());
    }
}
