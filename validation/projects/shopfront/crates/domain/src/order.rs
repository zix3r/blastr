//! Orders, their lines and the lifecycle a payment walks them through.

use std::fmt;

use crate::customer::{Address, Customer};
use crate::discount::Discount;
use crate::error::DomainError;
use crate::money::{Cents, Currency, Money};
use crate::product::Sku;

/// Database identity of an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderId(pub u64);

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Who is carrying the parcel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Carrier {
    Postal,
    Pickup,
    Courier { name: String, express: bool },
}

impl Carrier {
    /// Parses `postal`, `pickup` or `courier:<name>[:express]`.
    pub fn parse(spec: &str) -> Result<Carrier, DomainError> {
        let trimmed = spec.trim();
        let mut parts = trimmed.split(':');
        let kind = parts.next().unwrap_or_default().to_ascii_lowercase();
        let invalid = || DomainError::InvalidField {
            field: "carrier",
            value: trimmed.to_string(),
        };
        match kind.as_str() {
            "postal" => Ok(Carrier::Postal),
            "pickup" => Ok(Carrier::Pickup),
            "courier" => {
                let name = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
                if name.is_empty() {
                    return Err(invalid());
                }
                let express = match parts.next() {
                    Some(flag) => {
                        flag.eq_ignore_ascii_case("express") || flag.eq_ignore_ascii_case("true")
                    }
                    None => false,
                };
                Ok(Carrier::Courier { name, express })
            }
            _ => Err(invalid()),
        }
    }

    /// The short name used in reports.
    pub fn tag(&self) -> &str {
        match self {
            Carrier::Postal => "postal",
            Carrier::Pickup => "pickup",
            Carrier::Courier { name, .. } => name.as_str(),
        }
    }

    /// Whether the parcel is promised for the next day.
    pub fn is_express(&self) -> bool {
        match self {
            Carrier::Courier { express, .. } => *express,
            Carrier::Postal | Carrier::Pickup => false,
        }
    }

    /// The inverse of [`Carrier::parse`].
    pub fn spec(&self) -> String {
        match self {
            Carrier::Postal => "postal".to_string(),
            Carrier::Pickup => "pickup".to_string(),
            Carrier::Courier { name, express } => format!("courier:{name}:{express}"),
        }
    }
}

/// Why an order was called off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CancelReason {
    OutOfStock { sku: String },
    CustomerRequest,
    PaymentFailed { code: u32 },
}

impl CancelReason {
    /// Parses `out-of-stock:<sku>`, `request` or `payment:<code>`.
    pub fn parse(spec: &str) -> Result<CancelReason, DomainError> {
        let trimmed = spec.trim();
        let mut parts = trimmed.splitn(2, ':');
        let kind = parts.next().unwrap_or_default().to_ascii_lowercase();
        let rest = parts.next();
        let invalid = || DomainError::InvalidField {
            field: "reason",
            value: trimmed.to_string(),
        };
        match kind.as_str() {
            "out-of-stock" => match rest {
                Some(sku) if !sku.trim().is_empty() => Ok(CancelReason::OutOfStock {
                    sku: sku.trim().to_ascii_uppercase(),
                }),
                _ => Err(invalid()),
            },
            "request" => Ok(CancelReason::CustomerRequest),
            "payment" => {
                let code = rest
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(invalid)?;
                Ok(CancelReason::PaymentFailed { code })
            }
            _ => Err(invalid()),
        }
    }

    /// The inverse of [`CancelReason::parse`].
    pub fn spec(&self) -> String {
        match self {
            CancelReason::OutOfStock { sku } => format!("out-of-stock:{sku}"),
            CancelReason::CustomerRequest => "request".to_string(),
            CancelReason::PaymentFailed { code } => format!("payment:{code}"),
        }
    }

    /// A sentence for the packing slip.
    pub fn text(&self) -> String {
        match self {
            CancelReason::OutOfStock { sku } => format!("out of stock ({sku})"),
            CancelReason::CustomerRequest => "customer request".to_string(),
            CancelReason::PaymentFailed { code } => format!("payment failed (code {code})"),
        }
    }
}

/// Where an order sits in its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    /// Not yet submitted, only ever reached by the editor.
    Draft,
    /// Submitted and waiting for money.
    Placed,
    /// Money arrived; the reference is the payment processor's.
    Paid {
        reference: String,
        paid_cents: Cents,
    },
    /// Handed to a carrier.
    Shipped { carrier: Carrier, tracking: String },
    /// Arrived, `at_day` counts days since the order was placed.
    Delivered { at_day: u32 },
    /// Called off before shipping.
    Cancelled { reason: CancelReason },
    /// Money went back, only after delivery.
    Refunded { amount: Money, reason: CancelReason },
}

impl OrderStatus {
    /// The display name of the status.
    pub fn label(&self) -> &'static str {
        match self {
            OrderStatus::Draft => "Draft",
            OrderStatus::Placed => "Placed",
            OrderStatus::Paid { .. } => "Paid",
            OrderStatus::Shipped { .. } => "Shipped",
            OrderStatus::Delivered { .. } => "Delivered",
            OrderStatus::Cancelled { .. } => "Cancelled",
            OrderStatus::Refunded { .. } => "Refunded",
        }
    }

    /// One letter used by the operations report. Everything the shop treats as
    /// an exception (calls off and refunds) shares the catch-all letter.
    pub fn tag(&self) -> char {
        match self {
            OrderStatus::Draft => 'd',
            OrderStatus::Placed => 'p',
            OrderStatus::Paid { .. } => 'P',
            OrderStatus::Shipped { .. } => 's',
            OrderStatus::Delivered { .. } => 'D',
            _ => 'x',
        }
    }

    /// Whether the order still needs attention from the warehouse.
    pub fn is_open(&self) -> bool {
        match self {
            OrderStatus::Draft | OrderStatus::Placed | OrderStatus::Paid { .. } => true,
            OrderStatus::Shipped { .. }
            | OrderStatus::Delivered { .. }
            | OrderStatus::Cancelled { .. }
            | OrderStatus::Refunded { .. } => false,
        }
    }

    /// The inverse of [`OrderStatus::parse`].
    pub fn spec(&self) -> String {
        match self {
            OrderStatus::Draft => "draft".to_string(),
            OrderStatus::Placed => "placed".to_string(),
            OrderStatus::Paid {
                reference,
                paid_cents,
            } => format!("paid:{paid_cents}:{reference}"),
            OrderStatus::Shipped { carrier, tracking } => {
                format!("shipped:{}:{tracking}", carrier.spec())
            }
            OrderStatus::Delivered { at_day } => format!("delivered:{at_day}"),
            OrderStatus::Cancelled { reason } => format!("cancelled:{}", reason.spec()),
            OrderStatus::Refunded { amount, reason } => {
                format!("refunded:{}:{}", amount.cents, reason.spec())
            }
        }
    }

    /// Parses the spec produced by [`OrderStatus::spec`]; refunds and payments
    /// are amounts in `currency`.
    pub fn parse(spec: &str, currency: Currency) -> Result<OrderStatus, DomainError> {
        let trimmed = spec.trim();
        let fields: Vec<&str> = trimmed.split(':').collect();
        let kind = fields[0].trim().to_ascii_lowercase();
        let invalid = || DomainError::InvalidField {
            field: "status",
            value: trimmed.to_string(),
        };
        match (kind.as_str(), fields.len()) {
            ("draft", 1) => Ok(OrderStatus::Draft),
            ("placed", 1) => Ok(OrderStatus::Placed),
            ("paid", 3) => {
                let paid_cents: Cents = fields[1].parse().map_err(|_| invalid())?;
                Ok(OrderStatus::Paid {
                    reference: fields[2].to_string(),
                    paid_cents,
                })
            }
            ("shipped", len) if len >= 3 => {
                let carrier = Carrier::parse(&fields[1..len - 1].join(":"))?;
                Ok(OrderStatus::Shipped {
                    carrier,
                    tracking: fields[len - 1].to_string(),
                })
            }
            ("delivered", 2) => {
                let at_day: u32 = fields[1].parse().map_err(|_| invalid())?;
                Ok(OrderStatus::Delivered { at_day })
            }
            ("cancelled", len) if len >= 2 => Ok(OrderStatus::Cancelled {
                reason: CancelReason::parse(&fields[1..].join(":"))?,
            }),
            ("refunded", len) if len >= 2 => {
                let amount: Cents = fields[1].parse().map_err(|_| invalid())?;
                let reason = CancelReason::parse(&fields[2..].join(":"))?;
                Ok(OrderStatus::Refunded {
                    amount: Money::new(amount, currency),
                    reason,
                })
            }
            _ => Err(invalid()),
        }
    }

    /// The payment reference, when there is one.
    pub fn reference(&self) -> Option<&str> {
        match self {
            OrderStatus::Paid { reference, .. } => Some(reference.as_str()),
            OrderStatus::Draft
            | OrderStatus::Placed
            | OrderStatus::Shipped { .. }
            | OrderStatus::Delivered { .. }
            | OrderStatus::Cancelled { .. }
            | OrderStatus::Refunded { .. } => None,
        }
    }
}

/// One article in an order, with the catalogue data frozen at order time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderLine {
    pub sku: Sku,
    pub quantity: u32,
    pub unit_price: Money,
    pub weight_grams: u32,
    pub hazardous: bool,
    pub discount: Discount,
}

impl OrderLine {
    /// Builds a line without a reduction.
    pub fn new(sku: Sku, quantity: u32, unit_price: Money, weight_grams: u32) -> OrderLine {
        OrderLine {
            sku,
            quantity,
            unit_price,
            weight_grams,
            hazardous: false,
            discount: Discount::None,
        }
    }

    /// Attaches a reduction to the line.
    pub fn with_discount(mut self, discount: Discount) -> OrderLine {
        self.discount = discount;
        self
    }

    /// Marks the line as hazardous cargo.
    pub fn with_hazard(mut self, hazardous: bool) -> OrderLine {
        self.hazardous = hazardous;
        self
    }

    /// Shipping weight of the whole line.
    pub fn total_weight(&self) -> u32 {
        self.weight_grams.saturating_mul(self.quantity)
    }

    /// The price of the line before any reduction.
    pub fn gross(&self) -> Money {
        self.unit_price.scaled(self.quantity)
    }
}

/// A customer's basket, frozen and submitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: OrderId,
    pub customer: Customer,
    pub lines: Vec<OrderLine>,
    pub ship_to: Address,
    pub discount: Discount,
    pub status: OrderStatus,
    pub currency: Currency,
}

impl Order {
    /// A fresh draft in `currency`.
    pub fn new(id: OrderId, customer: Customer, ship_to: Address, currency: Currency) -> Order {
        Order {
            id,
            customer,
            lines: Vec::new(),
            ship_to,
            discount: Discount::None,
            status: OrderStatus::Draft,
            currency,
        }
    }

    /// Appends a line, refusing a line in another currency.
    pub fn add_line(&mut self, line: OrderLine) -> Result<(), DomainError> {
        if line.unit_price.currency != self.currency {
            return Err(DomainError::CurrencyMismatch {
                left: self.currency,
                right: line.unit_price.currency,
            });
        }
        self.lines.push(line);
        Ok(())
    }

    /// Total number of units in the order.
    pub fn total_quantity(&self) -> u32 {
        self.lines
            .iter()
            .fold(0_u32, |total, line| total.saturating_add(line.quantity))
    }

    /// Combined shipping weight in grams.
    pub fn total_weight(&self) -> u32 {
        self.lines.iter().fold(0_u32, |total, line| {
            total.saturating_add(line.total_weight())
        })
    }

    /// The line for `sku`, if the order has one.
    pub fn line_for(&self, sku: &Sku) -> Option<&OrderLine> {
        self.lines.iter().find(|line| &line.sku == sku)
    }

    /// Every SKU in the order, in insertion order.
    pub fn sku_list(&self) -> Vec<Sku> {
        self.lines.iter().map(|line| line.sku.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::customer::{Address, Country, CustomerId};
    use crate::money::{Cents, Currency};
    use crate::product::Sku;

    fn customer() -> Customer {
        Customer::new(
            CustomerId(7),
            "Ada",
            "ada@example.com",
            Address::new("Hauptstr 1", "Berlin", "10115", Country::De),
        )
    }

    fn order() -> Order {
        let address = customer().billing_address.clone();
        Order::new(OrderId(1000), customer(), address, Currency::Eur)
    }

    fn mug(qty: u32) -> OrderLine {
        OrderLine::new(
            Sku::parse("MUG-1").unwrap(),
            qty,
            Money::new(1299, Currency::Eur),
            400,
        )
    }

    #[test]
    fn order_collects_lines_and_totals() {
        let mut order = order();
        order.add_line(mug(2)).unwrap();
        order
            .add_line(OrderLine::new(
                Sku::parse("LAMP-2").unwrap(),
                1,
                Money::new(4000, Currency::Eur),
                1200,
            ))
            .unwrap();
        assert_eq!(order.total_quantity(), 3);
        assert_eq!(order.total_weight(), 2000);
        assert_eq!(order.sku_list().len(), 2);
        assert_eq!(
            order
                .line_for(&Sku::parse("MUG-1").unwrap())
                .map(|l| l.gross().cents),
            Some(2598)
        );
        assert_eq!(order.line_for(&Sku::parse("NOPE-9").unwrap()), None);
        assert_eq!(order.status, OrderStatus::Draft);
    }

    #[test]
    fn add_line_rejects_another_currency() {
        let mut order = order();
        let foreign = OrderLine::new(
            Sku::parse("MUG-1").unwrap(),
            1,
            Money::new(1299, Currency::Usd),
            400,
        );
        assert_eq!(
            order.add_line(foreign),
            Err(DomainError::CurrencyMismatch {
                left: Currency::Eur,
                right: Currency::Usd,
            })
        );
        assert!(order.lines.is_empty());
    }

    #[test]
    fn line_helpers() {
        let line = mug(3)
            .with_hazard(true)
            .with_discount(Discount::Percent { basis_points: 500 });
        assert_eq!(line.total_weight(), 1200);
        assert!(line.hazardous);
        assert_eq!(line.gross().cents, 3897);
    }

    #[test]
    fn status_specs_round_trip() {
        let statuses = [
            OrderStatus::Draft,
            OrderStatus::Placed,
            OrderStatus::Paid {
                reference: "PAY-1".to_string(),
                paid_cents: 8512,
            },
            OrderStatus::Shipped {
                carrier: Carrier::Courier {
                    name: "ups".to_string(),
                    express: true,
                },
                tracking: "1Z999".to_string(),
            },
            OrderStatus::Shipped {
                carrier: Carrier::Postal,
                tracking: "TRACK7".to_string(),
            },
            OrderStatus::Delivered { at_day: 3 },
            OrderStatus::Cancelled {
                reason: CancelReason::OutOfStock {
                    sku: "MUG-1".to_string(),
                },
            },
            OrderStatus::Refunded {
                amount: Money::new(8512, Currency::Eur),
                reason: CancelReason::PaymentFailed { code: 51 },
            },
        ];
        for status in statuses {
            let spec = status.spec();
            assert_eq!(OrderStatus::parse(&spec, Currency::Eur), Ok(status.clone()));
            assert!(status.tag().is_ascii_alphabetic());
        }
    }

    #[test]
    fn status_helpers_report_lifecycle() {
        assert_eq!(OrderStatus::Placed.label(), "Placed");
        assert!(
            OrderStatus::Paid {
                reference: "X".to_string(),
                paid_cents: 1
            }
            .is_open()
        );
        assert!(
            !OrderStatus::Cancelled {
                reason: CancelReason::CustomerRequest
            }
            .is_open()
        );
        assert_eq!(
            OrderStatus::Paid {
                reference: "PAY-9".to_string(),
                paid_cents: Cents::MAX
            }
            .reference(),
            Some("PAY-9")
        );
        assert_eq!(OrderStatus::Draft.reference(), None);
        assert_eq!(
            OrderStatus::Cancelled {
                reason: CancelReason::CustomerRequest
            }
            .tag(),
            'x'
        );
    }

    #[test]
    fn carriers_and_reasons_round_trip() {
        for spec in ["postal", "pickup", "courier:ups:true"] {
            let carrier = Carrier::parse(spec).unwrap();
            assert_eq!(carrier.spec(), spec);
        }
        assert_eq!(Carrier::parse("courier:ups:true").unwrap().tag(), "ups");
        assert!(Carrier::parse("courier:ups:true").unwrap().is_express());
        assert!(!Carrier::Postal.is_express());
        assert!(Carrier::parse("drone").is_err());
        assert!(Carrier::parse("courier:").is_err());

        for spec in ["out-of-stock:MUG-1", "request", "payment:51"] {
            let reason = CancelReason::parse(spec).unwrap();
            assert_eq!(reason.spec(), spec);
        }
        assert_eq!(
            CancelReason::OutOfStock {
                sku: "MUG-1".to_string()
            }
            .text(),
            "out of stock (MUG-1)"
        );
        assert!(CancelReason::parse("out-of-stock").is_err());
        assert!(CancelReason::parse("payment:x").is_err());
    }

    #[test]
    fn status_parse_rejects_bad_specs() {
        for spec in ["", "paid:1", "delivered:x", "fnord", "draft:1"] {
            assert!(OrderStatus::parse(spec, Currency::Eur).is_err(), "{spec}");
        }
    }
}
