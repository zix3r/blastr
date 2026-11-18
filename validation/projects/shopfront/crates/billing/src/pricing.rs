//! Line and order totals. This is the module the front end and the store both
//! call, so it is the deepest chain in the workspace.

use domain::{Cents, Currency, Money, Order, OrderLine, Region, sum_by};

use crate::discount;
use crate::shipping::shipping_cost;
use crate::tax::tax_for;

/// The price of a line before the line's own reduction.
pub fn line_gross(line: &OrderLine) -> Cents {
    line.gross().cents
}

/// The price of a line after the line's own reduction.
pub fn line_total(line: &OrderLine) -> Cents {
    let OrderLine {
        quantity, discount, ..
    } = line;
    discount::apply(line.gross(), discount, *quantity).cents
}

/// Every line after its own reduction, but before the order reduction.
pub fn order_subtotal(order: &Order) -> Cents {
    sum_by(&order.lines, line_total)
}

/// The complete money picture of an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Totals {
    /// Before any reduction.
    pub gross: Cents,
    /// After every reduction, before tax and shipping.
    pub net: Cents,
    /// What the reductions saved.
    pub discount: Cents,
    pub tax: Cents,
    pub shipping: Cents,
    pub grand_total: Cents,
    pub currency: Currency,
}

impl Totals {
    /// Wraps a raw amount in this invoice's currency.
    pub fn money(&self, cents: Cents) -> Money {
        Money::new(cents, self.currency)
    }
}

/// The taxing region of the customer that placed the order.
pub fn region_of(order: &Order) -> Region {
    order.customer.region()
}

/// Computes everything an invoice needs for `order`.
pub fn order_totals(order: &Order) -> Totals {
    let currency = order.currency;
    let gross = Money::new(sum_by(&order.lines, line_gross), currency);
    let net = discount::apply(
        Money::new(order_subtotal(order), currency),
        &order.discount,
        order.total_quantity(),
    );
    let region = region_of(order);
    let tax = Money::new(tax_for(region, net.cents), currency);
    let shipping = Money::new(shipping_cost(order, region, net.cents), currency);
    let grand_total = net
        .checked_add(tax)
        .and_then(|sum| sum.checked_add(shipping))
        .unwrap_or(Money::new(Cents::MAX, currency));
    Totals {
        gross: gross.cents,
        net: net.cents,
        discount: gross.cents - net.cents,
        tax: tax.cents,
        shipping: shipping.cents,
        grand_total: grand_total.cents,
        currency,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Address, Country, Customer, CustomerId, Discount, OrderId, OrderLine, Sku};

    fn order() -> Order {
        let customer = Customer::new(
            CustomerId(1),
            "Ada",
            "ada@example.com",
            Address::new("Hauptstr 1", "Berlin", "10115", Country::De),
        );
        let address = customer.billing_address.clone();
        Order::new(OrderId(1000), customer, address, Currency::Eur)
    }

    fn line(sku: &str, qty: u32, cents: Cents, weight: u32) -> OrderLine {
        OrderLine::new(
            Sku::parse(sku).unwrap(),
            qty,
            Money::new(cents, Currency::Eur),
            weight,
        )
    }

    #[test]
    fn line_gross_and_total_differ_only_by_the_reduction() {
        let plain = line("MUG-1", 2, 1299, 400);
        assert_eq!(line_gross(&plain), 2598);
        assert_eq!(line_total(&plain), 2598);

        let reduced = plain.with_discount(Discount::Percent { basis_points: 1000 });
        assert_eq!(line_gross(&reduced), 2598);
        assert_eq!(line_total(&reduced), 2338);
    }

    #[test]
    fn subtotal_adds_the_reduced_lines() {
        let mut order = order();
        order
            .add_line(
                line("MUG-1", 2, 1299, 400).with_discount(Discount::Percent { basis_points: 500 }),
            )
            .unwrap();
        order.add_line(line("LAMP-2", 1, 4000, 1200)).unwrap();
        assert_eq!(order_subtotal(&order), 2468 + 4000);
    }

    #[test]
    fn totals_follow_the_whole_rule_book() {
        let mut order = order();
        order.add_line(line("MUG-1", 2, 1299, 400)).unwrap();
        order.add_line(line("LAMP-2", 1, 4000, 1200)).unwrap();
        order.discount = Discount::Percent { basis_points: 1000 };
        let totals = order_totals(&order);
        assert_eq!(totals.gross, 6598);
        assert_eq!(totals.net, 5938);
        assert_eq!(totals.discount, 660);
        assert_eq!(totals.tax, 1128);
        assert_eq!(totals.shipping, 1439);
        assert_eq!(totals.grand_total, 8505);
        assert_eq!(totals.currency, Currency::Eur);
        assert_eq!(totals.money(totals.grand_total).format(), "85.05 EUR");
        assert_eq!(region_of(&order), Region::Europe);
    }

    #[test]
    fn empty_orders_have_no_totals() {
        let totals = order_totals(&order());
        assert_eq!(totals.gross, 0);
        assert_eq!(totals.net, 0);
        assert_eq!(totals.tax, 0);
        assert_eq!(totals.shipping, 0);
        assert_eq!(totals.grand_total, 0);
    }
}
