//! Tax tables. The rates live in a `const` table so the finance team can read
//! them without opening the calculator logic.

use domain::{Cents, Region, portion};

/// One taxing zone's rate and the label printed on invoices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaxBracket {
    pub region: Region,
    pub basis_points: u32,
    pub label: &'static str,
}

/// Orders at or below this net amount are not taxed at all.
pub const TAX_FREE_BELOW_CENTS: Cents = 500;

/// The published rate table.
pub const TAX_TABLE: [TaxBracket; 4] = [
    TaxBracket {
        region: Region::Domestic,
        basis_points: 2_000,
        label: "VAT 20%",
    },
    TaxBracket {
        region: Region::Europe,
        basis_points: 1_900,
        label: "VAT 19%",
    },
    TaxBracket {
        region: Region::NorthAmerica,
        basis_points: 850,
        label: "Sales tax 8.5%",
    },
    TaxBracket {
        region: Region::RestOfWorld,
        basis_points: 0,
        label: "No tax",
    },
];

/// The rate for `region`, in basis points.
pub fn tax_rate_bp(region: Region) -> u32 {
    match region {
        Region::Domestic => 2_000,
        Region::Europe => 1_900,
        Region::NorthAmerica => 850,
        Region::RestOfWorld => 0,
    }
}

/// The bracket for `region`; falls back to the untaxed row.
pub fn bracket_for(region: Region) -> TaxBracket {
    TAX_TABLE
        .into_iter()
        .find(|bracket| bracket.region == region)
        .unwrap_or(TAX_TABLE[3])
}

/// The label printed after `tax (` on an invoice.
pub fn label_for(region: Region) -> &'static str {
    bracket_for(region).label
}

/// Tax due on a net amount in `region`, rounded half away from zero.
pub fn tax_for(region: Region, net_cents: Cents) -> Cents {
    if net_cents <= TAX_FREE_BELOW_CENTS {
        return 0;
    }
    portion(net_cents, tax_rate_bp(region))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_the_rate_function() {
        for bracket in TAX_TABLE {
            assert_eq!(
                bracket.basis_points,
                tax_rate_bp(bracket.region),
                "table drifted for {}",
                bracket.region.label()
            );
            assert_eq!(bracket_for(bracket.region), bracket);
        }
    }

    #[test]
    fn small_orders_are_not_taxed() {
        assert_eq!(tax_for(Region::Domestic, TAX_FREE_BELOW_CENTS), 0);
        assert_eq!(tax_for(Region::Domestic, 0), 0);
        assert_eq!(tax_for(Region::RestOfWorld, 100_000), 0);
    }

    #[test]
    fn european_tax_is_nineteen_percent() {
        assert_eq!(tax_for(Region::Europe, 6532), 1241);
        assert_eq!(tax_for(Region::NorthAmerica, 1000), 85);
        assert_eq!(tax_for(Region::Domestic, 501), 100);
    }

    #[test]
    fn labels_are_published() {
        assert_eq!(label_for(Region::Domestic), "VAT 20%");
        assert_eq!(label_for(Region::Europe), "VAT 19%");
        assert_eq!(label_for(Region::NorthAmerica), "Sales tax 8.5%");
        assert_eq!(label_for(Region::RestOfWorld), "No tax");
    }
}
