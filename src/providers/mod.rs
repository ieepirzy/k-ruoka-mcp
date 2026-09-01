//! External catalogue providers that are independent of K-Ruoka's browser session.
//!
//! K-Ruoka remains under `browser/` because its private API is authenticated by the
//! persistent Chrome profile. Providers here have their own transport/session model.

pub mod alko;
