//! The catalogue: SKUs and the products behind them.

use std::fmt;

use crate::error::DomainError;
use crate::money::Money;

/// A stock keeping unit, always upper case and always with a group prefix.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sku(pub String);

impl Sku {
    /// Longest SKU the warehouse can print on a label.
    pub const MAX_LEN: usize = 16;

    /// Separates the catalogue group from the article number.
    pub const SEPARATOR: char = '-';

    /// Validates and normalises a SKU typed by a human.
    pub fn parse(raw: &str) -> Result<Sku, DomainError> {
        let normalised = raw.trim().to_ascii_uppercase();
        let reject = |reason: &str| DomainError::InvalidSku {
            raw: raw.to_string(),
            reason: reason.to_string(),
        };
        if normalised.is_empty() {
            return Err(reject("empty"));
        }
        if normalised.chars().count() > Sku::MAX_LEN {
            return Err(reject("too long"));
        }
        if !normalised
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == Sku::SEPARATOR)
        {
            return Err(reject("only A-Z, 0-9 and '-' are allowed"));
        }
        if !normalised.contains(Sku::SEPARATOR) {
            return Err(reject("missing group prefix"));
        }
        if normalised.starts_with(Sku::SEPARATOR) || normalised.ends_with(Sku::SEPARATOR) {
            return Err(reject("'-' may not lead or trail"));
        }
        Ok(Sku(normalised))
    }

    /// The raw code.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The catalogue group, i.e. everything before the first separator.
    pub fn group(&self) -> &str {
        self.0.split(Sku::SEPARATOR).next().unwrap_or(&self.0)
    }
}

impl fmt::Display for Sku {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A sellable article.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Product {
    pub sku: Sku,
    pub name: String,
    pub unit_price: Money,
    pub weight_grams: u32,
    pub hazardous: bool,
}

impl Product {
    /// Builds a regular, non hazardous product.
    pub fn new(sku: Sku, name: impl Into<String>, unit_price: Money, weight_grams: u32) -> Product {
        Product {
            sku,
            name: name.into(),
            unit_price,
            weight_grams,
            hazardous: false,
        }
    }

    /// Marks the product as needing special handling.
    pub fn with_hazard(mut self, hazardous: bool) -> Product {
        self.hazardous = hazardous;
        self
    }

    /// Shipping weight of `quantity` units of this product, in grams.
    pub fn weight_for(&self, quantity: u32) -> u32 {
        self.weight_grams.saturating_mul(quantity)
    }

    /// A one line description used by listings.
    pub fn describe(&self) -> String {
        format!("{} ({})", self.sku, self.name)
    }
}

/// The cheapest product of a catalogue slice, if there is one.
pub fn cheapest<'a>(products: impl IntoIterator<Item = &'a Product>) -> Option<&'a Product> {
    let mut best: Option<&Product> = None;
    for product in products {
        best = match best {
            Some(current) if current.unit_price.cents <= product.unit_price.cents => Some(current),
            _ => Some(product),
        };
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Currency;

    fn sample(sku: &str, cents: i64) -> Product {
        Product::new(
            Sku::parse(sku).unwrap(),
            "Widget",
            Money::new(cents, Currency::Eur),
            250,
        )
    }

    #[test]
    fn sku_parse_normalises_case_and_space() {
        assert_eq!(Sku::parse(" mug-1 ").unwrap().as_str(), "MUG-1");
        assert_eq!(Sku::parse("LAMP-22").unwrap().group(), "LAMP");
    }

    #[test]
    fn sku_parse_rejects_bad_input() {
        for raw in ["", "MUG", "MUG-", "-1", "MUG 1", "THIS-IS-MUCH-TOO-LONG"] {
            let error = Sku::parse(raw).unwrap_err();
            assert!(
                error.message().starts_with("invalid sku"),
                "unexpected message for {raw:?}: {error}"
            );
        }
    }

    #[test]
    fn product_weight_scales_with_quantity() {
        let product = sample("MUG-1", 1299);
        assert_eq!(product.weight_for(3), 750);
        assert_eq!(product.weight_for(0), 0);
        assert_eq!(product.describe(), "MUG-1 (Widget)");
        assert!(!product.hazardous);
        assert!(product.clone().with_hazard(true).hazardous);
    }

    #[test]
    fn cheapest_picks_the_lowest_price() {
        let cheap = sample("AAA-1", 100);
        let pricey = sample("BBB-1", 900);
        let products = [&pricey, &cheap];
        assert_eq!(cheapest(products).map(|p| p.sku.as_str()), Some("AAA-1"));
        assert_eq!(
            cheapest(std::iter::empty::<&Product>()).map(|p| p.sku.as_str()),
            None
        );
    }
}
