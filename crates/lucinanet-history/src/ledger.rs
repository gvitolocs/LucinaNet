//! When each history entry was left, so the policy can age them. Time is seconds on any monotonic clock the caller keeps.

struct Entry {
    url: String,
    /// Seconds when the entry stopped being the current one; meaningless while it is current.
    left_at: u64,
}

#[derive(Default)]
pub struct Ledger {
    entries: Vec<Entry>,
    current: usize,
}

impl Ledger {
    /// The engine's notification: the whole list and the current index, at `now`. An entry at the same place with the same URL
    /// is the same entry; the one that was current and no longer is gets `left_at = now`.
    pub fn update(&mut self, urls: &[String], current: usize, now: u64) {
        let mut next = Vec::with_capacity(urls.len());
        for (i, url) in urls.iter().enumerate() {
            let kept = self.entries.get(i).filter(|old| old.url == *url);
            let left_at = match kept {
                Some(_) if i == self.current && i != current => now,
                Some(old) => old.left_at,
                None => now,
            };
            next.push(Entry { url: url.clone(), left_at });
        }
        self.entries = next;
        self.current = current;
    }

    /// Ages in seconds of the entries behind the current one and ahead of it, each farthest first (the order `plan` takes:
    /// the closest entry, the one Back or Forward goes to, is last).
    pub fn ages(&self, now: u64) -> (Vec<u64>, Vec<u64>) {
        let age = |e: &Entry| now.saturating_sub(e.left_at);
        let past = self.entries.iter().take(self.current).map(age).collect();
        let future = self.entries.iter().skip(self.current + 1).rev().map(age).collect();
        (past, future)
    }
}
