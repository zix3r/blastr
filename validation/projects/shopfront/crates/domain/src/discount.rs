//! Reductions applied to a line or a whole order.

use crate::error::DomainError;
use crate::money::{Currency, Money};

/// The reduction schemes the shop can attach to an order or a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Discount {
    /// Nothing to take off.
    None,
    /// A percentage, quoted in basis points (10 % is 1000).
    Percent { basis_points: u32 },
    /// A fixed amount off, never more than the amount itself.
    FixedAmount(Money),
    /// Shipping is on the house.
    FreeShipping,
    /// A percentage that only kicks in from `min_quantity` units upwards.
    Bundle {
        code: String,
        basis_points: u32,
        min_quantity: u32,
    },
}

impl Discount {
    /// The spec used by scripts: `none`, `percent:10`, `bp:1250`, `fixed:500`,
    /// `freeship` or `bundle:SPRING:1000:3`. `fixed` amounts are taken in
    /// `currency`.
    pub fn parse(spec: &str, currency: Currency) -> Result<Discount, DomainError> {
        let trimmed = spec.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
            return Ok(Discount::None);
        }
        if trimmed.eq_ignore_ascii_case("freeship") || trimmed.eq_ignore_ascii_case("free-shipping")
        {
            return Ok(Discount::FreeShipping);
        }
        let mut parts = trimmed.split(':');
        let kind = parts.next().unwrap_or_default().to_ascii_lowercase();
        let invalid = || DomainError::InvalidDiscount(trimmed.to_string());
        match kind.as_str() {
            "percent" => {
                let whole: u32 = parts
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                if whole == 0 || whole > 100 {
                    return Err(invalid());
                }
                Ok(Discount::Percent {
                    basis_points: whole * 100,
                })
            }
            "bp" => {
                let basis_points: u32 = parts
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                if basis_points == 0 || basis_points > 10_000 {
                    return Err(invalid());
                }
                Ok(Discount::Percent { basis_points })
            }
            "fixed" => {
                let cents: i64 = parts
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                if cents <= 0 {
                    return Err(invalid());
                }
                Ok(Discount::FixedAmount(Money::new(cents, currency)))
            }
            "bundle" => {
                let code = parts.next().ok_or_else(invalid)?;
                let basis_points: u32 = parts
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                let min_quantity: u32 = parts
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?;
                if code.is_empty()
                    || basis_points == 0
                    || basis_points > 10_000
                    || min_quantity == 0
                {
                    return Err(invalid());
                }
                Ok(Discount::Bundle {
                    code: code.to_ascii_uppercase(),
                    basis_points,
                    min_quantity,
                })
            }
            _ => Err(invalid()),
        }
    }

    /// The inverse of [`Discount::parse`]; `fixed` amounts keep their cents.
    pub fn spec(&self) -> String {
        match self {
            Discount::None => "none".to_string(),
            Discount::Percent { basis_points } => format!("bp:{basis_points}"),
            Discount::FixedAmount(amount) => format!("fixed:{}", amount.cents),
            Discount::FreeShipping => "freeship".to_string(),
            Discount::Bundle {
                code,
                basis_points,
                min_quantity,
            } => format!("bundle:{code}:{basis_points}:{min_quantity}"),
        }
    }

    /// The percentage quoted in basis points, or zero for flat reductions.
    pub fn basis_points(&self) -> u32 {
        match self {
            Discount::Percent { basis_points } => *basis_points,
            Discount::Bundle { basis_points, .. } => *basis_points,
            Discount::None | Discount::FixedAmount(_) | Discount::FreeShipping => 0,
        }
    }

    /// Whether this reduction waives the shipping quote.
    pub fn affects_shipping(&self) -> bool {
        match self {
            Discount::FreeShipping => true,
            Discount::None | Discount::Percent { .. } => false,
            Discount::FixedAmount(_) => false,
            Discount::Bundle { .. } => false,
        }
    }

    /// The cut this reduction takes from `amount` for `quantity` units.
    ///
    /// Anything that is not a percentage and not a reached bundle threshold is
    /// handled by the fallback arm, which takes nothing.
    pub fn cut(&self, amount: Money, quantity: u32) -> Money {
        match self {
            Discount::Percent { basis_points } => amount.percent_bp(*basis_points),
            Discount::FixedAmount(off) => Money::new(off.cents.min(amount.cents), amount.currency),
            Discount::Bundle {
                basis_points,
                min_quantity,
                ..
            } if quantity >= *min_quantity => amount.percent_bp(*basis_points),
            _ => Money::zero(amount.currency),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Currency;

    fn eur(cents: i64) -> Money {
        Money::new(cents, Currency::Eur)
    }

    #[test]
    fn parse_understands_every_spelling() {
        assert_eq!(Discount::parse("", Currency::Eur), Ok(Discount::None));
        assert_eq!(Discount::parse("NONE", Currency::Eur), Ok(Discount::None));
        assert_eq!(
            Discount::parse("percent:10", Currency::Eur),
            Ok(Discount::Percent { basis_points: 1000 })
        );
        assert_eq!(
            Discount::parse("bp:1250", Currency::Eur),
            Ok(Discount::Percent { basis_points: 1250 })
        );
        assert_eq!(
            Discount::parse("fixed:500", Currency::Eur),
            Ok(Discount::FixedAmount(eur(500)))
        );
        assert_eq!(
            Discount::parse("freeship", Currency::Eur),
            Ok(Discount::FreeShipping)
        );
        assert_eq!(
            Discount::parse("bundle:spring:1000:3", Currency::Eur),
            Ok(Discount::Bundle {
                code: "SPRING".to_string(),
                basis_points: 1000,
                min_quantity: 3,
            })
        );
    }

    #[test]
    fn parse_rejects_out_of_range_values() {
        for spec in [
            "percent:0",
            "percent:101",
            "fixed:0",
            "fnord",
            "bundle:X:0:2",
        ] {
            let error = Discount::parse(spec, Currency::Eur).unwrap_err();
            assert_eq!(error.message(), format!("invalid discount '{spec}'"));
        }
    }

    #[test]
    fn specs_round_trip() {
        for spec in [
            "none",
            "bp:1000",
            "fixed:500",
            "freeship",
            "bundle:SPRING:1000:3",
        ] {
            let discount = Discount::parse(spec, Currency::Eur).unwrap();
            assert_eq!(discount.spec(), spec);
        }
    }

    #[test]
    fn cut_takes_a_share_and_never_exceeds_the_amount() {
        let percent = Discount::Percent { basis_points: 1000 };
        assert_eq!(percent.cut(eur(6598), 1).cents, 660);
        assert_eq!(
            Discount::FixedAmount(eur(50_000)).cut(eur(1299), 1).cents,
            1299
        );
        assert_eq!(Discount::None.cut(eur(1299), 4).cents, 0);
    }

    #[test]
    fn bundle_needs_the_threshold() {
        let bundle = Discount::Bundle {
            code: "SPRING".to_string(),
            basis_points: 500,
            min_quantity: 3,
        };
        assert_eq!(bundle.cut(eur(3000), 2).cents, 0);
        assert_eq!(bundle.cut(eur(3000), 3).cents, 150);
        assert_eq!(bundle.basis_points(), 500);
    }

    #[test]
    fn only_free_shipping_waives_shipping() {
        assert!(Discount::FreeShipping.affects_shipping());
        assert!(!Discount::None.affects_shipping());
        assert!(!Discount::Percent { basis_points: 100 }.affects_shipping());
        assert!(!Discount::FixedAmount(eur(10)).affects_shipping());
        assert_eq!(Discount::Percent { basis_points: 250 }.basis_points(), 250);
    }
}
