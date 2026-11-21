/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// MODEL, not the shipped code: how a video's pictures are handed to WebRender without a copy
// (crates/lucinanet-media/src/surfaces.rs `pin`/`unpin`/`Surface::publish`, crates/lucinanet-servo/src/media_images.rs). The shipped
// code is Arcs in a mutex-held registry and swgl textures pointed at their bytes; this mirrors its steps for one external image id.
//
// A lock of the id pins the picture the surface shows and points a swgl texture at its bytes (`set_texture_buffer`, no copy). The
// texture is one per (id, plane), shared by every open lock of the id, so a later lock re-points it at a newer picture. A lock may
// also find no picture (the player is gone, or has not shown one yet): WebRender's tile path still unlocks it at the end of the frame.
//
// The rule: while a lock of the id is open, every texture locked under it points at a picture that is pinned, so `publish` never
// writes into a buffer WebRender may read. Shipped (counted pins): an id's pins stay until its last open lock is let go.
// The last lemma shows that the first version broke the rule: there each unlock let go of the oldest pin, so a lock that found
// nothing, unlocked, freed the picture another open lock's texture still pointed at.
//
// Assumed, not proved: WebRender reads a texture only between its lock and its unlock and unlocks each lock once (renderer/
// external_image.rs; sw_compositor.rs unlocks only surfaces that locked); swgl keeps the pointer only; an Arc's count says who holds a
// buffer (`publish` takes one only when the pool is its only holder); every id's locks are all let go at the end of each frame.
use vstd::prelude::*;

verus! {

pub struct State {
    /// The picture the surface shows (`Registry::current`); None once the surface is dropped.
    pub current: Option<int>,
    /// Locks of the id `pin` counted.
    pub count: nat,
    /// Locks of the id WebRender has made and will unlock (each once).
    pub opened: nat,
    /// Pictures pinned under the id.
    pub held: Set<int>,
    /// Texture keys locked since the id's locks were last all let go: WebRender may read these.
    pub live: Set<int>,
    /// The picture each texture key points at.
    pub tex: Map<int, int>,
    /// The picture Servo's latest video frame holds.
    pub frame: Option<int>,
}

pub enum Step {
    /// `publish` into buffer `b`, then `current` and Servo's frame are `b`.
    Publish(int),
    /// A lock that found the current picture, its texture `k` pointed at it (`wrap`).
    Lock(int),
    /// A lock that found nothing: counted, as the tile path unlocks it.
    LockNothing,
    /// `unpin`.
    Unlock,
    /// The player drops its Surface.
    DropSurface,
    /// Servo drops its frame.
    DropFrame,
}

pub open spec fn writable(s: State, b: int) -> bool {
    s.current != Some(b) && s.frame != Some(b) && !s.held.contains(b)
}

pub open spec fn enabled(s: State, st: Step) -> bool {
    match st {
        Step::Publish(b) => writable(s, b),
        Step::Lock(_) => s.current is Some,
        Step::Unlock => s.opened > 0,
        _ => true,
    }
}

pub open spec fn apply(s: State, st: Step) -> State {
    match st {
        Step::Publish(b) => State { current: Some(b), frame: Some(b), ..s },
        Step::Lock(k) => State {
            count: s.count + 1,
            opened: s.opened + 1,
            held: s.held.insert(s.current->Some_0),
            live: s.live.insert(k),
            tex: s.tex.insert(k, s.current->Some_0),
            ..s
        },
        Step::LockNothing => State { count: s.count + 1, opened: s.opened + 1, ..s },
        Step::Unlock => if s.count == 1 {
            State { count: 0, opened: (s.opened - 1) as nat, held: Set::empty(), live: Set::empty(), ..s }
        } else {
            State { count: (s.count - 1) as nat, opened: (s.opened - 1) as nat, ..s }
        },
        Step::DropSurface => State { current: None, ..s },
        Step::DropFrame => State { frame: None, ..s },
    }
}

pub open spec fn holds(s: State) -> bool {
    &&& s.count == s.opened
    &&& (s.count == 0 ==> s.held.is_empty() && s.live.is_empty())
    &&& forall|k: int| #[trigger] s.live.contains(k) ==> s.tex.contains_key(k) && s.held.contains(s.tex[k])
}

pub open spec fn init() -> State {
    State { current: None, count: 0, opened: 0, held: Set::empty(), live: Set::empty(), tex: Map::empty(), frame: None }
}

proof fn init_holds()
    ensures holds(init()),
{
}

/// Every step keeps the rule.
proof fn step_keeps(s: State, st: Step)
    requires holds(s), enabled(s, st),
    ensures holds(apply(s, st)),
{
    let t = apply(s, st);
    match st {
        Step::Lock(k) => {
            let c = s.current->Some_0;
            assert forall|x: int| #[trigger] t.live.contains(x) implies t.tex.contains_key(x) && t.held.contains(t.tex[x]) by {
                if x != k {
                    assert(s.live.contains(x));
                }
            }
        },
        Step::Unlock => {
            if s.count == 1 {
                assert(t.held.is_empty());
            }
        },
        _ => {},
    }
}

/// What `publish` writes is never a picture an open lock's texture points at.
proof fn publish_never_writes_a_read_picture(s: State, b: int, k: int)
    requires holds(s), enabled(s, Step::Publish(b)), s.live.contains(k),
    ensures s.tex[k] != b,
{
}

/// Any run of enabled steps from the start keeps the rule.
pub open spec fn run(s: State, steps: Seq<Step>) -> Option<State>
    decreases steps.len(),
{
    if steps.len() == 0 {
        Some(s)
    } else if enabled(s, steps[0]) {
        run(apply(s, steps[0]), steps.drop_first())
    } else {
        None
    }
}

proof fn runs_keep(s: State, steps: Seq<Step>)
    requires holds(s),
    ensures run(s, steps) matches Some(t) ==> holds(t),
    decreases steps.len(),
{
    if steps.len() > 0 && enabled(s, steps[0]) {
        step_keeps(s, steps[0]);
        runs_keep(apply(s, steps[0]), steps.drop_first());
    }
}

/// The first version: one pin per lock that found a picture, each unlock lets go of the oldest.
pub struct Old {
    pub current: Option<int>,
    pub pins: Seq<int>,
    pub open: nat,
    pub live: Set<int>,
    pub tex: Map<int, int>,
}

pub open spec fn old_holds(s: Old) -> bool {
    s.open > 0 ==> forall|k: int| #[trigger] s.live.contains(k) ==> s.tex.contains_key(k) && s.pins.contains(s.tex[k])
}

pub open spec fn old_apply(s: Old, st: Step) -> Old {
    match st {
        Step::Lock(k) => Old {
            pins: s.pins.push(s.current->Some_0),
            open: s.open + 1,
            live: s.live.insert(k),
            tex: s.tex.insert(k, s.current->Some_0),
            ..s
        },
        Step::LockNothing => Old { open: s.open + 1, ..s },
        Step::Unlock => if s.open == 1 {
            Old { pins: if s.pins.len() > 0 { s.pins.drop_first() } else { s.pins }, open: 0, live: Set::empty(), ..s }
        } else {
            Old { pins: if s.pins.len() > 0 { s.pins.drop_first() } else { s.pins }, open: (s.open - 1) as nat, ..s }
        },
        _ => s,
    }
}

/// From a surface showing picture 1 with nothing locked: a lock that found it (texture 0), a lock that found nothing, then the
/// second one's unlock. Picture 1 is let go while the first lock is open and its texture points at it, and `publish` may then write
/// into it. Under the shipped rule the same steps keep it pinned.
proof fn old_unpin_breaks_the_rule()
{
    let s0 = Old { current: Some(1), pins: Seq::empty(), open: 0, live: Set::empty(), tex: Map::empty() };
    let s1 = old_apply(s0, Step::Lock(0));
    let s2 = old_apply(s1, Step::LockNothing);
    let s3 = old_apply(s2, Step::Unlock);
    assert(old_holds(s0));
    assert(s1.pins =~= seq![1int]);
    assert(s1.pins.contains(1)) by { assert(s1.pins[0] == 1); }
    assert forall|k: int| #[trigger] s1.live.contains(k) implies s1.tex.contains_key(k) && s1.pins.contains(s1.tex[k]) by {
        assert(k == 0);
    }
    assert(old_holds(s1));
    assert(s3.open == 1 && s3.live.contains(0) && s3.tex[0] == 1);
    assert(s3.pins.len() == 0);
    assert(!old_holds(s3));

    let n0 = State { current: Some(1), count: 0, opened: 0, held: Set::empty(), live: Set::empty(), tex: Map::empty(), frame: Some(1) };
    let n3 = apply(apply(apply(n0, Step::Lock(0)), Step::LockNothing), Step::Unlock);
    assert(n3.count == 1 && n3.live.contains(0) && n3.tex[0] == 1 && n3.held.contains(1));
    assert(holds(n3));
}

fn main() {}

} // verus!
