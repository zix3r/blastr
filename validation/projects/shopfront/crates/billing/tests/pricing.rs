//! Integration coverage for the pricing rules, driven through the public API
//! the CLI and the store both use.

use billing::{line_gross, line_total, order_subtotal, order_totals};
use domain::{
    Address, CancelReason, Country, Currency, Customer, CustomerId, Discount, DomainError, Money,
    Order, OrderId, OrderLine, OrderStatus, Region, Sku,
};

fn customer(country: Country) -> Customer {
    Customer::new(
        CustomerId(1),
        "Ada Lovelace",
        "ada@example.com",
        Address::new("Hauptstr 1", "Berlin", "10115", country),
    )
}

fn order(country: Country, currency: Currency) -> Order {
    let customer = customer(country);
    let address = customer.billing_address.clone();
    Order::new(OrderId(1000), customer, address, currency)
}

fn line(sku: &str, quantity: u32, cents: i64, weight: u32) -> OrderLine {
    OrderLine::new(
        Sku::parse(sku).unwrap(),
        quantity,
        Money::new(cents, Currency::Eur),
        weight,
    )
}

#[test]
fn free_shipping_starts_at_the_threshold() {
    let mut order = order(Country::De, Currency::Eur);
    order.add_line(line("MUG-1", 5, 1500, 400)).unwrap();
    let totals = order_totals(&order);
    assert_eq!(totals.gross, 7_500);
    assert_eq!(totals.net, 7_500);
    assert_eq!(totals.shipping, 0);
    assert_eq!(totals.tax, 1_425);
    assert_eq!(totals.grand_total, 8_925);
}

#[test]
fn a_bundle_reduction_needs_the_quantity() {
    let bundle = Discount::Bundle {
        code: "SPRING".to_string(),
        basis_points: 1_000,
        min_quantity: 3,
    };

    let mut short = order(Country::De, Currency::Eur);
    short.add_line(line("MUG-1", 2, 1299, 400)).unwrap();
    short.discount = bundle.clone();
    assert_eq!(order_totals(&short).discount, 0);

    let mut reached = order(Country::De, Currency::Eur);
    reached.add_line(line("MUG-1", 3, 1299, 400)).unwrap();
    reached.discount = bundle;
    let totals = order_totals(&reached);
    assert_eq!(totals.gross, 3_897);
    assert_eq!(totals.net, 3_507);
    assert_eq!(totals.discount, 390);
    assert_eq!(totals.tax, 666);
    assert_eq!(totals.shipping, 1_439);
    assert_eq!(totals.grand_total, 5_612);
}

#[test]
fn line_reductions_stack_before_the_order_reduction() {
    let mut order = order(Country::De, Currency::Eur);
    order
        .add_line(
            line("MUG-1", 2, 1299, 400).with_discount(Discount::Percent { basis_points: 500 }),
        )
        .unwrap();
    order.add_line(line("LAMP-2", 1, 4000, 1200)).unwrap();
    order.discount = Discount::FixedAmount(Money::new(200, Currency::Eur));

    let reduced = &order.lines[0];
    assert_eq!(line_gross(reduced), 2_598);
    assert_eq!(line_total(reduced), 2_468);
    assert_eq!(order_subtotal(&order), 6_468);

    let totals = order_totals(&order);
    assert_eq!(totals.gross, 6_598);
    assert_eq!(totals.net, 6_268);
    assert_eq!(totals.discount, 330);
}

#[test]
fn north_american_orders_use_the_sales_tax_bracket() {
    let mut order = order(Country::Us, Currency::Eur);
    order.add_line(line("MUG-1", 1, 1_000, 300)).unwrap();
    let totals = order_totals(&order);
    assert_eq!(totals.tax, 85);
    assert_eq!(totals.shipping, 499 + 120 + 700);
    assert_eq!(totals.grand_total, 2_404);
}

#[test]
fn untaxed_zones_add_no_tax_but_still_ship() {
    let mut order = order(Country::Other("JP".to_string()), Currency::Eur);
    order.add_line(line("MUG-1", 1, 2_000, 300)).unwrap();
    let totals = order_totals(&order);
    assert_eq!(order.customer.region(), Region::RestOfWorld);
    assert_eq!(totals.tax, 0);
    assert_eq!(totals.grand_total, 2_000 + 1_319);
}

#[test]
fn a_refund_note_does_not_change_the_totals() {
    let mut order = order(Country::Gb, Currency::Eur);
    order.add_line(line("MUG-1", 1, 1_299, 400)).unwrap();
    let before = order_totals(&order);
    order.status = OrderStatus::Refunded {
        amount: Money::new(before.grand_total, Currency::Eur),
        reason: CancelReason::CustomerRequest,
    };
    assert_eq!(order_totals(&order), before);
}

#[test]
fn lines_in_another_currency_are_refused() {
    let mut order = order(Country::De, Currency::Eur);
    let foreign = OrderLine::new(
        Sku::parse("MUG-1").unwrap(),
        1,
        Money::new(1_299, Currency::Usd),
        400,
    );
    assert_eq!(
        order.add_line(foreign),
        Err(DomainError::CurrencyMismatch {
            left: Currency::Eur,
            right: Currency::Usd,
        })
    );
}
