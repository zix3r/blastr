//! Integration coverage for the invoice renderer.

use billing::{generate_invoice, invoice_summary, order_totals};
use domain::{
    Address, Country, Currency, Customer, CustomerId, Discount, Money, Order, OrderId, OrderLine,
    OrderStatus, Sku,
};

fn shop_order() -> Order {
    let customer = Customer::new(
        CustomerId(1),
        "Ada Lovelace",
        "ada@example.com",
        Address::new("Hauptstr 1", "Berlin", "10115", Country::De),
    );
    let address = customer.billing_address.clone();
    Order::new(OrderId(1000), customer, address, Currency::Eur)
}

fn add(order: &mut Order, sku: &str, quantity: u32, cents: i64, weight: u32) {
    order
        .add_line(OrderLine::new(
            Sku::parse(sku).unwrap(),
            quantity,
            Money::new(cents, Currency::Eur),
            weight,
        ))
        .unwrap();
}

#[test]
fn a_two_line_invoice_renders_exactly() {
    let mut order = shop_order();
    add(&mut order, "MUG-1", 2, 1299, 400);
    add(&mut order, "LAMP-2", 1, 4000, 1200);
    order.discount = Discount::Percent {
        basis_points: 1_000,
    };
    order.status = OrderStatus::Placed;

    let totals = order_totals(&order);
    assert_eq!(totals.net, 5_938);
    assert_eq!(
        invoice_summary(order.id, &totals),
        "order #1000 total=85.05 EUR"
    );

    let expected = "\
INVOICE #1000
customer: Ada Lovelace <ada@example.com>
ship-to: Hauptstr 1, Berlin 10115 DE
destination: 10115 Berlin DE
status: Placed
line: 2 x MUG-1 @ 12.99 EUR = 25.98 EUR
line: 1 x LAMP-2 @ 40.00 EUR = 40.00 EUR
subtotal: 65.98 EUR
discount (10% off): -6.60 EUR
tax (VAT 19%): 11.28 EUR
shipping: 14.39 EUR
total: 85.05 EUR
";
    assert_eq!(generate_invoice(&order, &totals), expected);
}

#[test]
fn free_shipping_orders_show_a_zero_charge() {
    let mut order = shop_order();
    add(&mut order, "MUG-1", 6, 1299, 400);
    order.discount = Discount::FreeShipping;
    let totals = order_totals(&order);
    let text = generate_invoice(&order, &totals);
    assert!(text.contains("shipping: 0.00 EUR\n"));
    assert!(!text.contains("discount ("));
    assert_eq!(totals.shipping, 0);
    assert_eq!(totals.discount, 0);
    assert_eq!(totals.grand_total, 7_794 + 1_481);
}
