//! Customers, their addresses and the taxing region they fall into.

use std::fmt;

use crate::error::DomainError;
use crate::money::{CENTS_PER_UNIT, Cents};

/// Coarse taxing zones. Tax itself is a billing concern; the domain only knows
/// which zone a customer belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Region {
    Domestic,
    Europe,
    NorthAmerica,
    RestOfWorld,
}

impl Region {
    /// Every region, in the order reports group them.
    pub const ALL: [Region; 4] = [
        Region::Domestic,
        Region::Europe,
        Region::NorthAmerica,
        Region::RestOfWorld,
    ];

    /// The name shown on invoices and in reports.
    pub const fn label(self) -> &'static str {
        match self {
            Region::Domestic => "Domestic",
            Region::Europe => "Europe",
            Region::NorthAmerica => "NorthAmerica",
            Region::RestOfWorld => "RestOfWorld",
        }
    }

    /// Parses a label ignoring case.
    pub fn from_label(raw: &str) -> Result<Region, DomainError> {
        let wanted = raw.trim();
        Region::ALL
            .into_iter()
            .find(|region| region.label().eq_ignore_ascii_case(wanted))
            .ok_or_else(|| DomainError::UnknownRegion(wanted.to_string()))
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Shipping destinations the shop quotes for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Country {
    Us,
    Ca,
    De,
    Fr,
    Gb,
    /// Any two letter country the shop has no special handling for.
    Other(String),
}

impl Country {
    /// Parses a two letter code; unknown codes become [`Country::Other`].
    pub fn from_code(raw: &str) -> Result<Country, DomainError> {
        let code = raw.trim().to_ascii_uppercase();
        if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(DomainError::UnknownCountry(raw.to_string()));
        }
        match code.as_str() {
            "US" => Ok(Country::Us),
            "CA" => Ok(Country::Ca),
            "DE" => Ok(Country::De),
            "FR" => Ok(Country::Fr),
            "GB" => Ok(Country::Gb),
            _ => Ok(Country::Other(code)),
        }
    }

    /// The code the shop prints.
    pub fn code(&self) -> &str {
        match self {
            Country::Us => "US",
            Country::Ca => "CA",
            Country::De => "DE",
            Country::Fr => "FR",
            Country::Gb => "GB",
            Country::Other(code) => code.as_str(),
        }
    }

    /// The taxing zone this country sits in.
    pub fn region(&self) -> Region {
        match self {
            Country::Us | Country::Ca => Region::NorthAmerica,
            Country::De | Country::Fr => Region::Europe,
            Country::Gb => Region::Domestic,
            Country::Other(_) => Region::RestOfWorld,
        }
    }
}

/// A postal address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub line1: String,
    pub city: String,
    pub postal_code: String,
    pub country: Country,
}

impl Address {
    /// Builds an address from its parts.
    pub fn new(
        line1: impl Into<String>,
        city: impl Into<String>,
        postal_code: impl Into<String>,
        country: Country,
    ) -> Address {
        Address {
            line1: line1.into(),
            city: city.into(),
            postal_code: postal_code.into(),
            country,
        }
    }

    /// A single printable line, e.g. `Hauptstr 1, Berlin 10115 DE`.
    pub fn single_line(&self) -> String {
        format!(
            "{}, {} {} {}",
            self.line1,
            self.city,
            self.postal_code,
            self.country.code()
        )
    }
}

/// Database identity of a customer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CustomerId(pub u64);

impl fmt::Display for CustomerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A registered buyer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Customer {
    pub id: CustomerId,
    pub name: String,
    pub email: String,
    pub billing_address: Address,
    pub loyalty_points: u32,
}

impl Customer {
    /// Registers a customer with an empty loyalty account.
    pub fn new(
        id: CustomerId,
        name: impl Into<String>,
        email: impl Into<String>,
        billing_address: Address,
    ) -> Customer {
        Customer {
            id,
            name: name.into(),
            email: email.into(),
            billing_address,
            loyalty_points: 0,
        }
    }

    /// The taxing zone of the customer's billing address.
    pub fn region(&self) -> Region {
        self.billing_address.country.region()
    }

    /// The `Name <email>` form printed on invoices.
    pub fn contact_line(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }

    /// Credits one point per whole currency unit spent. Non positive spends
    /// earn nothing, and the counter saturates instead of wrapping.
    pub fn add_loyalty(&mut self, spend_cents: Cents) {
        if spend_cents <= 0 {
            return;
        }
        let earned = u32::try_from(spend_cents / CENTS_PER_UNIT).unwrap_or(u32::MAX);
        self.loyalty_points = self.loyalty_points.saturating_add(earned);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin() -> Address {
        Address::new("Hauptstr 1", "Berlin", "10115", Country::De)
    }

    #[test]
    fn countries_map_to_regions() {
        assert_eq!(Country::Us.region(), Region::NorthAmerica);
        assert_eq!(Country::Ca.region(), Region::NorthAmerica);
        assert_eq!(Country::De.region(), Region::Europe);
        assert_eq!(Country::Fr.region(), Region::Europe);
        assert_eq!(Country::Gb.region(), Region::Domestic);
        assert_eq!(
            Country::Other("JP".to_string()).region(),
            Region::RestOfWorld
        );
    }

    #[test]
    fn country_codes_round_trip() {
        assert_eq!(Country::from_code("de").unwrap().code(), "DE");
        assert_eq!(
            Country::from_code("jp").unwrap(),
            Country::Other("JP".to_string())
        );
        assert!(Country::from_code("DEU").is_err());
        assert!(Country::from_code("").is_err());
    }

    #[test]
    fn region_labels_round_trip() {
        for region in Region::ALL {
            assert_eq!(Region::from_label(region.label()), Ok(region));
            assert_eq!(region.to_string(), region.label());
        }
        assert!(Region::from_label("Mars").is_err());
    }

    #[test]
    fn address_renders_one_line() {
        assert_eq!(berlin().single_line(), "Hauptstr 1, Berlin 10115 DE");
    }

    #[test]
    fn customer_region_follows_the_billing_address() {
        let customer = Customer::new(CustomerId(1), "Ada", "ada@example.com", berlin());
        assert_eq!(customer.region(), Region::Europe);
        assert_eq!(customer.contact_line(), "Ada <ada@example.com>");
    }

    #[test]
    fn loyalty_counts_whole_units_only() {
        let mut customer = Customer::new(CustomerId(1), "Ada", "ada@example.com", berlin());
        customer.add_loyalty(8512);
        assert_eq!(customer.loyalty_points, 85);
        customer.add_loyalty(99);
        assert_eq!(customer.loyalty_points, 85);
        customer.add_loyalty(-500);
        assert_eq!(customer.loyalty_points, 85);
        customer.add_loyalty(Cents::MAX);
        assert_eq!(customer.loyalty_points, u32::MAX);
    }
}
