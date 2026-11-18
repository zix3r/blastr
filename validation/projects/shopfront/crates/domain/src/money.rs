//! Money is always an integer number of cents in exactly one currency.

use std::fmt;

use crate::error::DomainError;

/// Integer cents; the only representation of money in this workspace.
pub type Cents = i64;

/// Cents in one major currency unit.
pub const CENTS_PER_UNIT: Cents = 100;

/// One whole unit expressed in basis points.
pub const BASIS_POINTS_PER_UNIT: i64 = 10_000;

/// Rounds `numerator / denominator` half away from zero.
///
/// Money is never banked: 0.5 cents always tips the customer's way, which is
/// what the accounting export expects.
pub(crate) fn round_div(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0);
    if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        -((-numerator + denominator / 2) / denominator)
    }
}

/// `basis_points` of `cents`, rounded half away from zero.
///
/// 10 000 basis points is one whole unit, so `portion(1299, 1000)` is 10 % of
/// 12.99 and returns 130.
pub fn portion(cents: Cents, basis_points: u32) -> Cents {
    let numerator = i128::from(cents) * i128::from(basis_points);
    round_div(numerator, i128::from(BASIS_POINTS_PER_UNIT)) as Cents
}

/// Sums an amount derived from every element, saturating on overflow.
pub fn sum_by<T, F>(items: &[T], mut amount_of: F) -> Cents
where
    F: FnMut(&T) -> Cents,
{
    let mut total: Cents = 0;
    for item in items {
        total = total.saturating_add(amount_of(item));
    }
    total
}

/// The currencies the shop is willing to trade in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Currency {
    Usd,
    Eur,
    Gbp,
}

impl Currency {
    /// Every supported currency, in reporting order.
    pub const ALL: [Currency; 3] = [Currency::Usd, Currency::Eur, Currency::Gbp];

    /// The ISO 4217 code.
    pub const fn code(self) -> &'static str {
        match self {
            Currency::Usd => "USD",
            Currency::Eur => "EUR",
            Currency::Gbp => "GBP",
        }
    }

    /// The glyph used on printed labels.
    pub const fn symbol(self) -> &'static str {
        match self {
            Currency::Usd => "$",
            Currency::Eur => "\u{20ac}",
            Currency::Gbp => "\u{a3}",
        }
    }

    /// Parses an ISO code, ignoring case and surrounding space.
    pub fn from_code(raw: &str) -> Result<Currency, DomainError> {
        let code = raw.trim().to_ascii_uppercase();
        Currency::ALL
            .into_iter()
            .find(|currency| currency.code() == code)
            .ok_or(DomainError::UnknownCurrency(code))
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// An amount of money: cents plus the currency they are cents of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Money {
    pub cents: Cents,
    pub currency: Currency,
}

impl Money {
    /// Builds an amount from raw cents.
    pub const fn new(cents: Cents, currency: Currency) -> Money {
        Money { cents, currency }
    }

    /// Zero in the given currency.
    pub const fn zero(currency: Currency) -> Money {
        Money { cents: 0, currency }
    }

    /// Adds two amounts, refusing to mix currencies.
    pub fn checked_add(self, other: Money) -> Result<Money, DomainError> {
        if self.currency != other.currency {
            return Err(DomainError::CurrencyMismatch {
                left: self.currency,
                right: other.currency,
            });
        }
        let cents = self
            .cents
            .checked_add(other.cents)
            .ok_or(DomainError::Overflow)?;
        Ok(Money {
            cents,
            currency: self.currency,
        })
    }

    /// Subtracts `other`, refusing to mix currencies.
    pub fn checked_sub(self, other: Money) -> Result<Money, DomainError> {
        if self.currency != other.currency {
            return Err(DomainError::CurrencyMismatch {
                left: self.currency,
                right: other.currency,
            });
        }
        let cents = self
            .cents
            .checked_sub(other.cents)
            .ok_or(DomainError::Overflow)?;
        Ok(Money {
            cents,
            currency: self.currency,
        })
    }

    /// Multiplies by a whole number of units.
    pub fn scaled(self, units: u32) -> Money {
        Money {
            cents: self.cents.saturating_mul(Cents::from(units)),
            currency: self.currency,
        }
    }

    /// The given fraction of the amount, rounded half away from zero.
    pub fn percent_bp(self, basis_points: u32) -> Money {
        Money {
            cents: portion(self.cents, basis_points),
            currency: self.currency,
        }
    }

    /// Whether the amount is exactly zero.
    pub fn is_zero(self) -> bool {
        self.cents == 0
    }

    /// Whether the amount is below zero.
    pub fn is_negative(self) -> bool {
        self.cents < 0
    }

    /// Renders the amount as `"12.99 EUR"`.
    pub fn format(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.cents < 0 { "-" } else { "" };
        let absolute = self.cents.unsigned_abs();
        write!(
            f,
            "{sign}{}.{:02} {}",
            absolute / 100,
            absolute % 100,
            self.currency.code()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DomainError;

    #[test]
    fn portion_rounds_half_away_from_zero() {
        assert_eq!(portion(1299, 1000), 130);
        assert_eq!(portion(1000, 1000), 100);
        assert_eq!(portion(5, 1000), 1);
        assert_eq!(portion(4, 1000), 0);
        assert_eq!(portion(-1299, 1000), -130);
        assert_eq!(portion(0, 5000), 0);
    }

    #[test]
    fn portion_handles_whole_units() {
        assert_eq!(portion(6598, 10_000), 6598);
        assert_eq!(portion(6598, 19_000), 12_536);
    }

    #[test]
    fn money_formats_two_decimals() {
        let amount = Money::new(1299, Currency::Eur);
        assert_eq!(amount.format(), "12.99 EUR");
        assert_eq!(Money::new(5, Currency::Gbp).format(), "0.05 GBP");
        assert_eq!(Money::new(-260, Currency::Usd).format(), "-2.60 USD");
        assert_eq!(Money::zero(Currency::Eur).format(), "0.00 EUR");
    }

    #[test]
    fn money_adds_and_subtracts_within_a_currency() {
        let a = Money::new(1500, Currency::Eur);
        let b = Money::new(250, Currency::Eur);
        assert_eq!(a.checked_add(b).unwrap().cents, 1750);
        assert_eq!(a.checked_sub(b).unwrap().cents, 1250);
        assert_eq!(
            a.checked_add(Money::new(1, Currency::Usd)),
            Err(DomainError::CurrencyMismatch {
                left: Currency::Eur,
                right: Currency::Usd,
            })
        );
        assert_eq!(
            Money::new(Cents::MAX, Currency::Eur).checked_add(Money::new(1, Currency::Eur)),
            Err(DomainError::Overflow)
        );
    }

    #[test]
    fn money_scales_and_takes_percentages() {
        let price = Money::new(1299, Currency::Eur);
        assert_eq!(price.scaled(3).cents, 3897);
        assert_eq!(price.scaled(0).cents, 0);
        assert_eq!(price.percent_bp(1000).cents, 130);
        assert!(Money::zero(Currency::Eur).is_zero());
        assert!(Money::new(-1, Currency::Eur).is_negative());
    }

    #[test]
    fn sum_by_adds_a_derived_amount() {
        let quantities = [1_u32, 2, 4];
        assert_eq!(sum_by(&quantities, |qty| Cents::from(*qty) * 100), 700);
        assert_eq!(sum_by::<u32, _>(&[], |qty| Cents::from(*qty)), 0);
    }

    #[test]
    fn currency_codes_round_trip() {
        assert_eq!(Currency::from_code("eur"), Ok(Currency::Eur));
        assert_eq!(Currency::from_code(" GBP "), Ok(Currency::Gbp));
        assert_eq!(
            Currency::from_code("xxx"),
            Err(DomainError::UnknownCurrency("XXX".to_string()))
        );
        assert_eq!(Currency::Eur.symbol(), "\u{20ac}");
        assert_eq!(Currency::ALL.len(), 3);
    }
}
