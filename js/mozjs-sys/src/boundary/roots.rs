// The root frames of our glue: when the engine calls a Servo native, the `vp` array (callee, this, arguments) must stay where
// it is while the native runs, nested natives included, and the GC must see and update every value in it, as SpiderMonkey's
// own stack frames are. The values live in one fixed array that never reallocates (`engine::cx`); this file decides which part of
// it each call gets and which part the GC walks. Verus checks this very file (`scripts/verify-shipped.sh`).
//
// Proved: frames are handed out last in, first out; a frame lies inside the array and inside what the GC walks, and never
// overlaps an older frame; popping gives back exactly the frame on top and walks the GC limit back to its start; a full
// array refuses a frame (the caller throws "too much recursion") instead of growing. Not proved: that every caller pops what it
// pushed (the engine does it in a guard's `drop`), and the GC's own walk.

pub struct FrameStack {
    /// Values in use, from 0: the GC walks `0..top`.
    top: usize,
    /// Size of the fixed array.
    capacity: usize,
    /// Start of each live frame, oldest first.
    starts: Vec<usize>,
}

impl FrameStack {
    //@ pub closed spec fn wf(&self) -> bool {
    //@     self.top <= self.capacity
    //@     && (forall|i: int| 0 <= i < self.starts@.len() ==> #[trigger] self.starts@[i] <= self.top)
    //@     && (forall|i: int, j: int| 0 <= i < j < self.starts@.len() ==> #[trigger] self.starts@[i] <= #[trigger] self.starts@[j])
    //@ }
    //@ pub closed spec fn top_spec(&self) -> usize { self.top }
    //@ pub closed spec fn capacity_spec(&self) -> usize { self.capacity }
    //@ pub closed spec fn depth_spec(&self) -> nat { self.starts@.len() }
    //@ pub closed spec fn starts_spec(&self) -> Seq<usize> { self.starts@ }

    pub fn new(capacity: usize) -> Self //@ret
    //@ ensures ret.wf(), ret.top_spec() == 0, ret.capacity_spec() == capacity, ret.depth_spec() == 0,
    {
        FrameStack { top: 0, capacity, starts: Vec::new() }
    }

    /// How many values the GC must walk: `0..walk_len()`.
    pub fn walk_len(&self) -> usize //@ret
    //@ requires self.wf(),
    //@ ensures ret == self.top_spec(), ret <= self.capacity_spec(),
    {
        self.top
    }

    pub fn depth(&self) -> usize //@ret
    //@ ensures ret == self.depth_spec(),
    {
        self.starts.len()
    }

    /// A new frame of `len` values on top: its start, or `None` if the array is full (nothing changes then).
    pub fn push(&mut self, len: usize) -> Option<usize> //@ret
    //@ requires old(self).wf(),
    //@ ensures
    //@     final(self).wf(),
    //@     final(self).capacity_spec() == old(self).capacity_spec(),
    //@     match ret {
    //@         Some(start) => start == old(self).top_spec() && final(self).top_spec() == start + len
    //@             && start + len <= final(self).capacity_spec() && final(self).depth_spec() == old(self).depth_spec() + 1
    //@             && final(self).starts_spec() == old(self).starts_spec().push(start),
    //@         None => *final(self) == *old(self) && old(self).top_spec() + len > old(self).capacity_spec(),
    //@     },
    {
        if len > self.capacity - self.top {
            return None;
        }
        let start = self.top;
        self.starts.push(start);
        self.top = start + len;
        //@ assert(forall|i: int| 0 <= i < self.starts@.len() ==> #[trigger] self.starts@[i] <= self.top);
        Some(start)
    }

    /// Pops the frame on top: its start, and the GC limit goes back to it. `None` if there is no frame.
    pub fn pop(&mut self) -> Option<usize> //@ret
    //@ requires old(self).wf(),
    //@ ensures
    //@     final(self).wf(),
    //@     final(self).capacity_spec() == old(self).capacity_spec(),
    //@     match ret {
    //@         Some(start) => old(self).depth_spec() > 0 && start == old(self).starts_spec().last()
    //@             && final(self).top_spec() == start && final(self).starts_spec() == old(self).starts_spec().drop_last(),
    //@         None => old(self).depth_spec() == 0 && final(self).top_spec() == old(self).top_spec() && final(self).starts_spec() == old(self).starts_spec(),
    //@     },
    {
        match self.starts.pop() {
            None => None,
            Some(start) => {
                self.top = start;
                //@ assert(forall|i: int| 0 <= i < self.starts@.len() ==> #[trigger] self.starts@[i] <= old(self).starts_spec().last());
                Some(start)
            }
        }
    }

    /// The range of the frame `depth` frames below the top (0 = top): inside the walked part, and older frames end where newer
    /// ones start, so a native's `vp` never overlaps another's.
    pub fn frame(&self, below_top: usize) -> Option<(usize, usize)> //@ret
    //@ requires self.wf(),
    //@ ensures match ret {
    //@     Some((s, e)) => s <= e && e <= self.top_spec() && below_top < self.depth_spec()
    //@         && s == self.starts_spec()[self.depth_spec() - 1 - below_top]
    //@         && e == (if below_top == 0 { self.top_spec() } else { self.starts_spec()[self.depth_spec() - below_top] }),
    //@     None => below_top >= self.depth_spec(),
    //@ },
    {
        let n = self.starts.len();
        if below_top >= n {
            return None;
        }
        let i = n - 1 - below_top;
        let start = self.starts[i];
        let end = if i + 1 < n { self.starts[i + 1] } else { self.top };
        Some((start, end))
    }
}
