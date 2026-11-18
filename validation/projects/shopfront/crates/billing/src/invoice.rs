//! Renders an order as the plain text invoice the shop emails out.

use domain::order::OrderId;
use domain::{Address, Discount, Money, Order, OrderStatus};

use crate::discount::{describe, rebate};
use crate::pricing::{Totals, line_gross, line_total};
use crate::tax::label_for;

/// The note appended below the totals for orders that ended badly.
fn status_note(status: &OrderStatus) -> String {
    match status {
        OrderStatus::Cancelled { reason } => format!("note: cancelled - {}", reason.text()),
        OrderStatus::Refunded { amount, reason } => {
            format!("note: refunded {} - {}", amount.format(), reason.text())
        }
        _ => String::new(),
    }
}

/// The whole invoice as a newline terminated text block.
pub fn generate_invoice(order: &Order, totals: &Totals) -> String {
    let mut out = String::new();
    out.push_str(&format!("INVOICE #{}\n", order.id));
    out.push_str(&format!("customer: {}\n", order.customer.contact_line()));
    out.push_str(&format!("ship-to: {}\n", order.ship_to.single_line()));

    let Address {
        city,
        postal_code,
        country,
        ..
    } = &order.ship_to;
    out.push_str(&format!(
        "destination: {} {} {}\n",
        postal_code,
        city,
        country.code()
    ));

    out.push_str(&format!("status: {}\n", order.status.label()));
    for line in &order.lines {
        out.push_str(&format!(
            "line: {} x {} @ {} = {}\n",
            line.quantity,
            line.sku,
            totals.money(line.unit_price.cents).format(),
            totals.money(line_total(line)).format()
        ));
        if line.discount != Discount::None {
            let cut = rebate(
                totals.money(line_gross(line)),
                &line.discount,
                line.quantity,
            );
            out.push_str(&format!(
                "line-discount ({}): -{}\n",
                describe(&line.discount),
                totals.money(cut).format()
            ));
        }
    }
    out.push_str(&format!(
        "subtotal: {}\n",
        totals.money(totals.gross).format()
    ));
    if totals.discount > 0 {
        out.push_str(&format!(
            "discount ({}): -{}\n",
            describe(&order.discount),
            totals.money(totals.discount).format()
        ));
    }
    out.push_str(&format!(
        "tax ({}): {}\n",
        label_for(order.customer.region()),
        totals.money(totals.tax).format()
    ));
    out.push_str(&format!(
        "shipping: {}\n",
        totals.money(totals.shipping).format()
    ));
    out.push_str(&format!(
        "total: {}\n",
        totals.money(totals.grand_total).format()
    ));

    let note = status_note(&order.status);
    if !note.is_empty() {
        out.push_str(&note);
        out.push('\n');
    }
    out
}

/// A one line summary used by the CLI when an order is placed.
pub fn invoice_summary(id: OrderId, totals: &Totals) -> String {
    format!(
        "order #{} total={}",
        id,
        Money::new(totals.grand_total, totals.currency).format()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::order_totals;
    use domain::{Country, Currency, Customer, CustomerId, OrderLine, Sku};

    fn order() -> Order {
        let customer = Customer::new(
            CustomerId(1),
            "Ada Lovelace",
            "ada@example.com",
            Address::new("Hauptstr 1", "Berlin", "10115", Country::De),
        );
        let address = customer.billing_address.clone();
        Order::new(OrderId(1000), customer, address, Currency::Eur)
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
    fn invoice_has_a_line_per_article_and_a_total() {
        let mut order = order();
        order.add_line(mug(2)).unwrap();
        order.status = OrderStatus::Placed;
        let totals = order_totals(&order);
        let text = generate_invoice(&order, &totals);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "INVOICE #1000");
        assert_eq!(lines[1], "customer: Ada Lovelace <ada@example.com>");
        assert_eq!(lines[2], "ship-to: Hauptstr 1, Berlin 10115 DE");
        assert_eq!(lines[3], "destination: 10115 Berlin DE");
        assert_eq!(lines[4], "status: Placed");
        assert_eq!(lines[5], "line: 2 x MUG-1 @ 12.99 EUR = 25.98 EUR");
        assert_eq!(lines[6], "subtotal: 25.98 EUR");
        assert_eq!(lines[7], "tax (VAT 19%): 4.94 EUR");
        assert_eq!(lines[8], "shipping: 13.19 EUR");
        assert_eq!(lines[9], "total: 44.11 EUR");
        assert_eq!(lines.len(), 10);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn line_discounts_are_shown_separately() {
        let mut order = order();
        order
            .add_line(mug(2).with_discount(Discount::Percent { basis_points: 1000 }))
            .unwrap();
        let totals = order_totals(&order);
        let text = generate_invoice(&order, &totals);
        assert!(text.contains("line: 2 x MUG-1 @ 12.99 EUR = 23.38 EUR\n"));
        assert!(text.contains("line-discount (10% off): -2.60 EUR\n"));
    }

    #[test]
    fn cancellations_get_a_note() {
        let mut order = order();
        order.add_line(mug(1)).unwrap();
        order.status = OrderStatus::Cancelled {
            reason: domain::CancelReason::OutOfStock {
                sku: "MUG-1".to_string(),
            },
        };
        let totals = order_totals(&order);
        let text = generate_invoice(&order, &totals);
        assert!(text.ends_with("note: cancelled - out of stock (MUG-1)\n"));
        assert_eq!(
            invoice_summary(order.id, &totals),
            "order #1000 total=28.65 EUR"
        );
    }
}
