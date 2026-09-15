//! What paid work costs, in millionths of a USDC (USDC has six decimals).
//!
//! Test-network prices only need to be roughly right. File work is priced by
//! the time it should take, from layer 1's `estimate`: about 0.66 ms per work
//! unit, the one rate measured so far (an image on the layer 1 bench). Measure
//! sound and video before real money.

/// The least any paid call costs: half a cent.
pub const FLOOR: u64 = 5_000;

/// Milliseconds of rendering per work unit.
pub const MS_PER_WORK_UNIT: f64 = 0.66;

/// What a millisecond of rendering costs.
pub const PER_MS: u64 = 1;

/// An edit changes JSON and packs files; it costs the floor.
pub const EDIT: u64 = FLOOR;

/// The price of file work of this size.
pub fn file_work(work_units: u64) -> u64 {
    let ms = (work_units as f64 * MS_PER_WORK_UNIT).ceil() as u64;
    ms.saturating_mul(PER_MS).max(FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_work_costs_the_floor_and_big_work_scales() {
        assert_eq!(file_work(0), FLOOR);
        assert_eq!(file_work(1_000), FLOOR);
        assert_eq!(file_work(100_000), 66_000);
        assert!(file_work(u64::MAX) > FLOOR);
    }
}
