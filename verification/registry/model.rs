// MODEL, not the shipped code: the order in which the client storage registry (Servo components/storage/client_storage.rs,
// our RedbEngine) creates and deletes an IndexedDB database's directory and its row, and the sweep at startup. The shipped code does
// file system calls and redb commits, which Verus does not run; this mirrors their order, step for step.
//
// The rule: a committed row always names a directory that exists, whatever step a power cut stops at, and after the sweep at
// startup no directory is left without a row. Servo committed the row first and created the directory after; the last lemma shows
// that this order breaks the rule (a cut between the two leaves a row for a directory that is not there, and IndexedDB then fails
// to open that database for good).
//
// Assumed, not proved: that a redb commit made with write_durable is on disk when it returns, and that a directory created and its
// parent synced is on disk (tests/power_cut.rs in the store and the QEMU power cuts test the first; Juno's ext4 the second); that
// the registry thread runs one operation at a time (it is one thread reading one channel).
use vstd::prelude::*;

verus! {

pub struct Disk {
    pub rows: Set<int>,
    pub dirs: Set<int>,
}

pub open spec fn holds(d: Disk) -> bool {
    d.rows.subset_of(d.dirs)
}

pub enum Step {
    MakeDir(int),
    CommitRow(int),
    CommitRemoval(int),
    RemoveDir(int),
}

pub open spec fn apply(d: Disk, s: Step) -> Disk {
    match s {
        Step::MakeDir(p) => Disk { rows: d.rows, dirs: d.dirs.insert(p) },
        Step::CommitRow(p) => Disk { rows: d.rows.insert(p), dirs: d.dirs },
        Step::CommitRemoval(p) => Disk { rows: d.rows.remove(p), dirs: d.dirs },
        Step::RemoveDir(p) => Disk { rows: d.rows, dirs: d.dirs.remove(p) },
    }
}

/// The first `n` steps of `steps`, from `d`: a cut after step `n`.
pub open spec fn run(d: Disk, steps: Seq<Step>, n: nat) -> Disk
    decreases n,
{
    if n == 0 || steps.len() == 0 {
        d
    } else if n > steps.len() {
        run(d, steps, steps.len() as nat)
    } else {
        apply(run(d, steps, (n - 1) as nat), steps[n - 1])
    }
}

/// create_database: the directory, synced, then the row (write_durable).
pub open spec fn create(p: int) -> Seq<Step> {
    seq![Step::MakeDir(p), Step::CommitRow(p)]
}

/// delete_database: the row's removal (write_durable), then the directory, as Servo.
pub open spec fn delete(p: int) -> Seq<Step> {
    seq![Step::CommitRemoval(p), Step::RemoveDir(p)]
}

/// At startup: every directory under bottles/ that no row names is removed.
pub open spec fn sweep(d: Disk) -> Disk {
    Disk { rows: d.rows, dirs: d.dirs.filter(|p: int| d.rows.contains(p)) }
}

pub proof fn create_holds_at_every_cut(d: Disk, p: int, n: nat)
    requires holds(d),
    ensures holds(run(d, create(p), n)),
{
    reveal_with_fuel(run, 3);
    assert(create(p).len() == 2);
    if n >= 1 {
        let one = run(d, create(p), 1);
        assert(one == apply(d, Step::MakeDir(p)));
        assert(holds(one));
        if n >= 2 {
            let two = run(d, create(p), 2);
            assert(two == apply(one, Step::CommitRow(p)));
            assert(one.dirs.contains(p));
            assert(holds(two));
        }
    }
}

pub proof fn delete_holds_at_every_cut(d: Disk, p: int, n: nat)
    requires holds(d),
    ensures holds(run(d, delete(p), n)),
{
    reveal_with_fuel(run, 3);
    assert(delete(p).len() == 2);
    if n >= 1 {
        let one = run(d, delete(p), 1);
        assert(one == apply(d, Step::CommitRemoval(p)));
        assert(holds(one));
        if n >= 2 {
            let two = run(d, delete(p), 2);
            assert(two == apply(one, Step::RemoveDir(p)));
            assert(!one.rows.contains(p));
            assert(holds(two));
        }
    }
}

/// One operation after another, with a cut anywhere in the last one: create when `ops[i].0`, delete otherwise.
pub open spec fn after(d: Disk, ops: Seq<(bool, int)>) -> Disk
    decreases ops.len(),
{
    if ops.len() == 0 {
        d
    } else {
        let last = ops[ops.len() - 1];
        let before = after(d, ops.drop_last());
        run(before, if last.0 { create(last.1) } else { delete(last.1) }, 2)
    }
}

pub proof fn holds_after_whole_ops(d: Disk, ops: Seq<(bool, int)>)
    requires holds(d),
    ensures holds(after(d, ops)),
    decreases ops.len(),
{
    if ops.len() > 0 {
        holds_after_whole_ops(d, ops.drop_last());
        let last = ops[ops.len() - 1];
        let before = after(d, ops.drop_last());
        if last.0 {
            create_holds_at_every_cut(before, last.1, 2);
        } else {
            delete_holds_at_every_cut(before, last.1, 2);
        }
    }
}

/// Any history of whole operations, then a cut at any step of one more: the rule holds, and after the sweep no directory lacks a row.
pub proof fn power_cut_anywhere(d: Disk, ops: Seq<(bool, int)>, create_next: bool, p: int, n: nat)
    requires holds(d),
    ensures
        holds(run(after(d, ops), if create_next { create(p) } else { delete(p) }, n)),
        holds(sweep(run(after(d, ops), if create_next { create(p) } else { delete(p) }, n))),
        sweep(run(after(d, ops), if create_next { create(p) } else { delete(p) }, n)).dirs
            =~= sweep(run(after(d, ops), if create_next { create(p) } else { delete(p) }, n)).rows,
{
    holds_after_whole_ops(d, ops);
    let before = after(d, ops);
    if create_next {
        create_holds_at_every_cut(before, p, n);
    } else {
        delete_holds_at_every_cut(before, p, n);
    }
}

/// Servo's order: the row first, then the directory. A cut between them breaks the rule.
pub open spec fn servo_create(p: int) -> Seq<Step> {
    seq![Step::CommitRow(p), Step::MakeDir(p)]
}

pub proof fn servo_order_breaks_the_rule()
    ensures !holds(run(Disk { rows: Set::empty(), dirs: Set::empty() }, servo_create(7), 1)),
{
    reveal_with_fuel(run, 2);
    let d = Disk { rows: Set::empty(), dirs: Set::empty() };
    let one = run(d, servo_create(7), 1);
    assert(one == apply(d, Step::CommitRow(7)));
    assert(one.rows.contains(7));
    assert(!one.dirs.contains(7));
}

} // verus!

fn main() {}
