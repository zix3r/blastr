//! Reductions: what a [`Discount`] is worth on a given amount.

use domain::{Cents, Discount, Money};

/// Applies `discount` to `amount` for a line of `quantity` units.
///
/// The result is never negative: a flat reduction larger than the amount is
/// clipped to zero.
pub fn apply(amount: Money, discount: &Discount, quantity: u32) -> Money {
    let cut = discount.cut(amount, quantity);
    amount
        .checked_sub(cut)
        .unwrap_or(Money::zero(amount.currency))
}

/// The rebate in cents that `discount` grants on `amount`.
pub fn rebate(amount: Money, discount: &Discount, quantity: u32) -> Cents {
    discount.cut(amount, quantity).cents
}

/// A short human description, e.g. `10% off` or `SPRING bundle 10% off 3+`.
pub fn describe(discount: &Discount) -> String {
    match discount {
        Discount::None => "no discount".to_string(),
        Discount::Percent { basis_points } => format!("{} off", percent_text(*basis_points)),
        Discount::FixedAmount(amount) => format!("{} off", amount.format()),
        Discount::FreeShipping => "free shipping".to_string(),
        Discount::Bundle {
            code,
            basis_points,
            min_quantity,
        } => format!(
            "{code} bundle {} off {min_quantity}+",
            percent_text(*basis_points)
        ),
    }
}

/// Renders basis points the way the price list does: `10%`, `12.50%`.
pub(crate) fn percent_text(basis_points: u32) -> String {
    let whole = basis_points / 100;
    let fraction = basis_points % 100;
    if fraction == 0 {
        format!("{whole}%")
    } else {
        format!("{whole}.{fraction:02}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Currency;

    fn eur(cents: Cents) -> Money {
        Money::new(cents, Currency::Eur)
    }

    #[test]
    fn percent_reduction_rounds_half_up() {
        let discounted = apply(eur(6598), &Discount::Percent { basis_points: 1000 }, 1);
        assert_eq!(discounted.cents, 5938);
        assert_eq!(
            rebate(eur(6598), &Discount::Percent { basis_points: 1000 }, 1),
            660
        );
    }

    #[test]
    fn flat_reduction_is_clipped() {
        let huge = Discount::FixedAmount(eur(500_000));
        assert_eq!(apply(eur(1299), &huge, 1).cents, 0);
        assert_eq!(rebate(eur(1299), &huge, 1), 1299);
    }

    #[test]
    fn none_and_free_shipping_leave_the_amount_alone() {
        assert_eq!(apply(eur(1299), &Discount::None, 4).cents, 1299);
        assert_eq!(apply(eur(1299), &Discount::FreeShipping, 4).cents, 1299);
        assert_eq!(rebate(eur(1299), &Discount::None, 4), 0);
    }

    #[test]
    fn bundle_only_applies_from_the_threshold() {
        let bundle = Discount::Bundle {
            code: "SPRING".to_string(),
            basis_points: 1000,
            min_quantity: 3,
        };
        assert_eq!(apply(eur(4000), &bundle, 2).cents, 4000);
        assert_eq!(apply(eur(4000), &bundle, 3).cents, 3600);
    }

    #[test]
    fn descriptions_are_stable() {
        assert_eq!(describe(&Discount::None), "no discount");
        assert_eq!(
            describe(&Discount::Percent { basis_points: 1000 }),
            "10% off"
        );
        assert_eq!(
            describe(&Discount::Percent { basis_points: 1250 }),
            "12.50% off"
        );
        assert_eq!(describe(&Discount::FreeShipping), "free shipping");
        assert_eq!(describe(&Discount::FixedAmount(eur(500))), "5.00 EUR off");
        assert_eq!(
            describe(&Discount::Bundle {
                code: "SPRING".to_string(),
                basis_points: 500,
                min_quantity: 3
            }),
            "SPRING bundle 5% off 3+"
        );
    }
}
