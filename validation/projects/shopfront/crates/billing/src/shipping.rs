//! Shipping quotes: weight bands, surcharges and the free shipping rule.

use domain::{Cents, Order, Region};

use crate::{
    BASE_SHIPPING_CENTS, FREE_SHIPPING_THRESHOLD_CENTS, HAZARD_SURCHARGE_CENTS,
    INTERNATIONAL_SURCHARGE_CENTS, PER_KG_SHIPPING_CENTS,
};

/// Whole started kilograms; 1 gram still costs a kilogram band.
pub(crate) fn kilograms(grams: u32) -> Cents {
    (Cents::from(grams) + 999) / 1000
}

/// Whether the order or any of its lines waives the shipping quote.
pub fn has_free_shipping(order: &Order) -> bool {
    order.discount.affects_shipping()
        || order
            .lines
            .iter()
            .any(|line| line.discount.affects_shipping())
}

/// Whether the parcel carries hazardous goods.
pub fn has_hazard(order: &Order) -> bool {
    order.lines.iter().any(|line| line.hazardous)
}

/// The shipping charge for `order` leaving to `region`, given the order's net
/// amount. Empty orders and orders above the free shipping threshold ship for
/// nothing, as do orders carrying a free shipping reduction.
pub fn shipping_cost(order: &Order, region: Region, net_cents: Cents) -> Cents {
    if order.lines.is_empty() || net_cents <= 0 {
        return 0;
    }
    if has_free_shipping(order) || net_cents >= FREE_SHIPPING_THRESHOLD_CENTS {
        return 0;
    }
    let mut cost = BASE_SHIPPING_CENTS + PER_KG_SHIPPING_CENTS * kilograms(order.total_weight());
    if has_hazard(order) {
        cost += HAZARD_SURCHARGE_CENTS;
    }
    if region != Region::Domestic {
        cost += INTERNATIONAL_SURCHARGE_CENTS;
    }
    cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{
        Address, Country, Currency, Customer, CustomerId, Discount, Money, OrderId, OrderLine, Sku,
    };

    fn customer(country: Country) -> Customer {
        Customer::new(
            CustomerId(1),
            "Ada",
            "ada@example.com",
            Address::new("Hauptstr 1", "Berlin", "10115", country),
        )
    }

    fn order_with(country: Country, lines: Vec<OrderLine>) -> Order {
        let customer = customer(country);
        let address = customer.billing_address.clone();
        let mut order = Order::new(OrderId(1), customer, address, Currency::Eur);
        for line in lines {
            order.add_line(line).unwrap();
        }
        order
    }

    fn line(sku: &str, qty: u32, weight: u32) -> OrderLine {
        OrderLine::new(
            Sku::parse(sku).unwrap(),
            qty,
            Money::new(1000, Currency::Eur),
            weight,
        )
    }

    #[test]
    fn kilograms_round_up() {
        assert_eq!(kilograms(0), 0);
        assert_eq!(kilograms(1), 1);
        assert_eq!(kilograms(1000), 1);
        assert_eq!(kilograms(1001), 2);
        assert_eq!(kilograms(2000), 2);
    }

    #[test]
    fn empty_orders_ship_for_free() {
        let order = order_with(Country::De, Vec::new());
        assert_eq!(shipping_cost(&order, Region::Europe, 0), 0);
        assert!(!has_free_shipping(&order));
        assert!(!has_hazard(&order));
    }

    #[test]
    fn weight_and_region_drive_the_quote() {
        let order = order_with(
            Country::De,
            vec![line("MUG-1", 2, 400), line("LAMP-2", 1, 1200)],
        );
        assert_eq!(order.total_weight(), 2000);
        assert_eq!(shipping_cost(&order, Region::Europe, 6532), 1439);
        assert_eq!(shipping_cost(&order, Region::Domestic, 6532), 739);
    }

    #[test]
    fn hazardous_goods_add_a_surcharge() {
        let order = order_with(Country::Us, vec![line("FUEL-1", 1, 500).with_hazard(true)]);
        assert!(has_hazard(&order));
        assert_eq!(
            shipping_cost(&order, Region::NorthAmerica, 1000),
            BASE_SHIPPING_CENTS
                + PER_KG_SHIPPING_CENTS
                + HAZARD_SURCHARGE_CENTS
                + INTERNATIONAL_SURCHARGE_CENTS
        );
    }

    #[test]
    fn large_and_free_shipping_orders_pay_nothing() {
        let large = order_with(Country::De, vec![line("MUG-1", 1, 400)]);
        assert_eq!(shipping_cost(&large, Region::Europe, 7_500), 0);

        let mut cheap = order_with(Country::De, vec![line("MUG-1", 1, 400)]);
        cheap.discount = Discount::FreeShipping;
        assert!(has_free_shipping(&cheap));
        assert_eq!(shipping_cost(&cheap, Region::Europe, 1000), 0);

        let mut line_waived = order_with(
            Country::De,
            vec![line("MUG-1", 1, 400).with_discount(Discount::FreeShipping)],
        );
        line_waived.discount = Discount::None;
        assert!(has_free_shipping(&line_waived));
        assert_eq!(shipping_cost(&line_waived, Region::Europe, 1000), 0);
    }
}
