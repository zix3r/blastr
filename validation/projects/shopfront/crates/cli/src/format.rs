//! Output formatting shared by the command handlers.

use domain::{Cents, Currency, Customer, Money, OrderId, OrderStatus, Product};
use storage::RegionRevenue;

/// Renders cents as `"12.99 EUR"`.
pub fn money(cents: Cents, currency: Currency) -> String {
    Money::new(cents, currency).format()
}

/// The line printed after `add-product`.
pub fn product_line(product: &Product, stock: u32) -> String {
    format!(
        "added product {} ({}) {} stock={}",
        product.sku,
        product.name,
        product.unit_price.format(),
        stock
    )
}

/// The line printed after `add-customer`.
pub fn customer_line(customer: &Customer) -> String {
    format!(
        "added customer {} {} region={} points={}",
        customer.id,
        customer.name,
        customer.region().label(),
        customer.loyalty_points
    )
}

/// The heading of the `report` command.
pub fn report_header(products: usize, customers: usize, orders: usize) -> String {
    format!("report products={products} customers={customers} orders={orders}")
}

/// One revenue row of the `report` command.
pub fn revenue_line(row: &RegionRevenue) -> String {
    format!(
        "revenue: {} orders={} total={}",
        row.region.label(),
        row.orders,
        money(row.total_cents, row.currency)
    )
}

/// One low stock row of the `report` command.
pub fn low_stock_line(product: &Product, stock: u32) -> String {
    format!("low-stock: {} stock={}", product.sku, stock)
}

/// The line printed after `advance`.
pub fn status_line(id: OrderId, status: &OrderStatus, currency: Currency) -> String {
    format!(
        "order #{} status={} ({})",
        id,
        status.label().to_ascii_lowercase(),
        status_detail(status, currency)
    )
}

/// A human sentence about a status, used inside [`status_line`].
pub fn status_detail(status: &OrderStatus, currency: Currency) -> String {
    match status {
        OrderStatus::Draft => "not submitted".to_string(),
        OrderStatus::Placed => "awaiting payment".to_string(),
        OrderStatus::Paid {
            reference,
            paid_cents,
        } => format!("paid {} via {reference}", money(*paid_cents, currency)),
        OrderStatus::Shipped { carrier, tracking } => {
            format!("handed to {} as {tracking}", carrier.tag())
        }
        OrderStatus::Delivered { at_day } => format!("delivered after {at_day} days"),
        OrderStatus::Cancelled { reason } => format!("cancelled: {}", reason.text()),
        OrderStatus::Refunded { amount, reason } => {
            format!("refunded {}: {}", amount.format(), reason.text())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{
        Address, CancelReason, Carrier, Country, CustomerId, Discount, OrderLine, Region, Sku,
    };

    fn product() -> Product {
        Product::new(
            Sku::parse("MUG-1").unwrap(),
            "Blue Mug",
            Money::new(1299, Currency::Eur),
            400,
        )
    }

    #[test]
    fn product_and_customer_lines_are_stable() {
        assert_eq!(
            product_line(&product(), 25),
            "added product MUG-1 (Blue Mug) 12.99 EUR stock=25"
        );
        let customer = Customer::new(
            CustomerId(1),
            "Ada Lovelace",
            "ada@example.com",
            Address::new("Hauptstr 1", "Berlin", "10115", Country::De),
        );
        assert_eq!(
            customer_line(&customer),
            "added customer 1 Ada Lovelace region=Europe points=0"
        );
    }

    #[test]
    fn report_rows_are_stable() {
        assert_eq!(
            report_header(2, 1, 1),
            "report products=2 customers=1 orders=1"
        );
        assert_eq!(
            revenue_line(&RegionRevenue {
                region: Region::Europe,
                orders: 1,
                total_cents: 8505,
                currency: Currency::Eur,
            }),
            "revenue: Europe orders=1 total=85.05 EUR"
        );
        assert_eq!(low_stock_line(&product(), 3), "low-stock: MUG-1 stock=3");
    }

    #[test]
    fn status_lines_cover_every_state() {
        assert_eq!(
            status_line(OrderId(1000), &OrderStatus::Placed, Currency::Eur),
            "order #1000 status=placed (awaiting payment)"
        );
        assert_eq!(
            status_detail(
                &OrderStatus::Paid {
                    reference: "PAY-1".to_string(),
                    paid_cents: 8505,
                },
                Currency::Eur
            ),
            "paid 85.05 EUR via PAY-1"
        );
        assert_eq!(
            status_detail(
                &OrderStatus::Shipped {
                    carrier: Carrier::Courier {
                        name: "ups".to_string(),
                        express: true
                    },
                    tracking: "1Z9".to_string(),
                },
                Currency::Eur
            ),
            "handed to ups as 1Z9"
        );
        assert_eq!(
            status_detail(&OrderStatus::Delivered { at_day: 3 }, Currency::Eur),
            "delivered after 3 days"
        );
        assert_eq!(
            status_detail(
                &OrderStatus::Cancelled {
                    reason: CancelReason::CustomerRequest
                },
                Currency::Eur
            ),
            "cancelled: customer request"
        );
        assert_eq!(
            status_detail(
                &OrderStatus::Refunded {
                    amount: Money::new(1299, Currency::Eur),
                    reason: CancelReason::PaymentFailed { code: 51 },
                },
                Currency::Eur
            ),
            "refunded 12.99 EUR: payment failed (code 51)"
        );
        assert_eq!(
            status_detail(&OrderStatus::Draft, Currency::Eur),
            "not submitted"
        );
        let line = OrderLine::new(
            Sku::parse("MUG-1").unwrap(),
            1,
            Money::new(1, Currency::Eur),
            1,
        )
        .with_discount(Discount::None);
        assert_eq!(line.discount, Discount::None);
    }
}
