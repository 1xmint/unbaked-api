//! Taking x402 payments for work: verify the payment, do the work, and settle
//! only if the work succeeded. See [`gate`] for the rules.

pub mod facilitator;
#[cfg(feature = "fake")]
pub mod fake;
pub mod gate;
pub mod wire;

pub use facilitator::{Facilitator, HttpFacilitator, Rejected};
pub use gate::{Gate, Quote, Terms};
pub use wire::{Requirements, Resource};
