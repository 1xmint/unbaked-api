//! The session's spending cap, held down by reservation so a price is never
//! signed for before the budget can cover it.

use std::sync::Mutex;

pub struct Budget {
    cap: u64,
    remaining: Mutex<u64>,
}

impl Budget {
    pub fn new(cap: u64) -> Self {
        Self {
            cap,
            remaining: Mutex::new(cap),
        }
    }

    pub fn cap(&self) -> u64 {
        self.cap
    }

    pub fn remaining(&self) -> u64 {
        *self.remaining.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Reserves `amount` from what is left, atomically. `Err` carries what
    /// was left, for the refusal message.
    pub fn reserve(&self, amount: u64) -> Result<Reservation<'_>, u64> {
        let mut remaining = self.remaining.lock().unwrap_or_else(|p| p.into_inner());
        if amount > *remaining {
            return Err(*remaining);
        }
        *remaining -= amount;
        Ok(Reservation {
            budget: self,
            amount,
            spent: false,
        })
    }
}

/// A hold against the budget. Dropped without [`Reservation::spend`], it
/// gives the amount back.
pub struct Reservation<'a> {
    budget: &'a Budget,
    amount: u64,
    spent: bool,
}

impl Reservation<'_> {
    pub fn amount(&self) -> u64 {
        self.amount
    }

    /// The reservation becomes permanent: the amount is gone for good.
    pub fn spend(mut self) {
        self.spent = true;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.spent {
            let mut remaining = self
                .budget
                .remaining
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *remaining += self.amount;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reservation_not_spent_is_released_on_drop() {
        let budget = Budget::new(1_000_000);
        {
            let reservation = budget.reserve(400_000).unwrap();
            assert_eq!(budget.remaining(), 600_000);
            drop(reservation);
        }
        assert_eq!(budget.remaining(), 1_000_000);
    }

    #[test]
    fn a_spent_reservation_stays_gone() {
        let budget = Budget::new(1_000_000);
        let reservation = budget.reserve(400_000).unwrap();
        reservation.spend();
        assert_eq!(budget.remaining(), 600_000);
    }

    #[test]
    fn over_budget_is_refused_before_reserving() {
        let budget = Budget::new(1_000_000);
        let _hold = budget.reserve(900_000).unwrap();
        assert_eq!(budget.reserve(200_000).err().unwrap(), 100_000);
        assert_eq!(budget.remaining(), 100_000);
    }
}
