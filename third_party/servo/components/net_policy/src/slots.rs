// An origin's connection slots and the queue for them. Verus checks this very file (`//@` lines).
#![allow(clippy::needless_return)] //@omit

/// Connections an origin may have at once: Chromium g_max_sockets_per_group = 6 for normal pools (net/socket/
/// client_socket_pool_manager.cc); Firefox network.http.max-persistent-connections-per-server = 6 (modules/libpref/init/all.js).
pub const MAX_PER_ORIGIN: usize = 6;

/// Whether a connection can be made now: fewer open than the limit.
pub fn has_room(live: usize, max: usize) -> bool //@ret
//@ ensures ret == (live < max),
{
    return live < max;
}

/// A waiter may take a slot only at the front of the queue and with room: first come, first served, and never over the limit.
pub fn may_take(position: usize, live: usize, max: usize) -> bool //@ret
//@ ensures ret == (position == 0 && live < max),
{
    return position == 0 && live < max;
}

/// Slots in use after one is taken.
pub fn taken(live: usize, max: usize) -> usize //@ret
//@ requires live < max, ensures ret == live + 1, ret <= max,
{
    return live + 1;
}

/// Slots in use after one is given back. A release with nothing in use changes nothing, so a double release cannot go below 0.
pub fn released(live: usize) -> usize //@ret
//@ ensures live > 0 ==> ret == live - 1, live == 0 ==> ret == 0, ret <= live,
{
    if live == 0 {
        return 0;
    }
    return live - 1;
}

/// A waiter's place in the queue after the waiter at `leaver` left it (served, or its request was dropped): those behind it
/// move up one, those ahead stay. The order of the others never changes.
pub fn position_after_leave(position: usize, leaver: usize) -> usize //@ret
//@ requires position != leaver, ensures position > leaver ==> ret == position - 1, position < leaver ==> ret == position, position > leaver ==> ret >= leaver,
{
    if position > leaver {
        return position - 1;
    }
    return position;
}
