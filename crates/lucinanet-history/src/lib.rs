//! The session-history depth policy (the history-depth spec). `policy` is pure integer rules, written so Verus can read
//! the very file (`//@` lines, see raster/verify.sh); `ledger` remembers when each history entry was left.
pub mod ledger;
pub mod policy;
