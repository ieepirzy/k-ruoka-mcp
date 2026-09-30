//! Additional Finnish catalogue providers.
//!
//! K-Ruoka remains under `browser/` because its private API and account state are the
//! core of this project. Alko is HTTP-only; S-Kaupat uses the shared Chrome process only
//! to discover frontend persisted-query hashes, then keeps catalogue traffic on HTTP.

pub mod alko;
pub mod s_kaupat;
