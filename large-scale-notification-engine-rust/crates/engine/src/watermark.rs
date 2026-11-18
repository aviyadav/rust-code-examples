use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

/// Tracks out-of-order task completion per partition and emits only monotonically
/// advancing, gap-free commit targets. This replaces the article's "simplified"
/// per-message commit (which is unsafe with concurrent workers).
#[derive(Default)]
struct PState {
    /// Lowest offset not yet contiguously completed == next offset to consume.
    base: i64,
    done: BTreeSet<i64>,
}

#[derive(Default)]
pub struct WatermarkTracker {
    inner: Mutex<HashMap<i32, PState>>,
}

pub enum TrackerEvent {
    Dispatched(i32, i64),
    Completed(i32, i64),
}

impl WatermarkTracker {
    pub fn dispatched(&self, p: i32, o: i64) {
        let mut g = self.inner.lock().unwrap();
        g.entry(p).or_insert(PState { base: o, done: BTreeSet::new() });
    }

    /// Returns Some(next_offset_to_commit) only when the contiguous prefix advanced.
    pub fn completed(&self, p: i32, o: i64) -> Option<i64> {
        let mut g = self.inner.lock().unwrap();
        let st = g.get_mut(&p)?;
        if o < st.base {
            return None; // stale completion below the watermark
        }
        st.done.insert(o);
        let old = st.base;
        while st.done.remove(&st.base) {
            st.base += 1;
        }
        if st.base > old { Some(st.base) } else { None }
    }

    /// Best-effort flush targets for rebalance/shutdown. Committing `base` is always
    /// safe: it is the next *unconsumed* offset (nothing unprocessed is skipped).
    pub fn flush_targets(&self) -> Vec<(i32, i64)> {
        self.inner.lock().unwrap().iter().map(|(p, s)| (*p, s.base)).collect()
    }

    pub fn flush_target(&self, p: i32) -> Option<i64> {
        self.inner.lock().unwrap().get(&p).map(|s| s.base)
    }

    pub fn revoke(&self, partitions: &[i32]) {
        let mut g = self.inner.lock().unwrap();
        for p in partitions {
            g.remove(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_completion_advances_only_contiguous_prefix() {
        let t = WatermarkTracker::default();
        for o in 0..4 { t.dispatched(0, o); }
        assert_eq!(t.completed(0, 2), None);
        assert_eq!(t.completed(0, 0), Some(1));
        assert_eq!(t.completed(0, 1), Some(3)); // 2 was already done
        assert_eq!(t.completed(0, 3), Some(4));
        assert_eq!(t.completed(0, 0), None);    // stale
    }
}