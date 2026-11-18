//! Integration coverage for the snapshot format.

use billing::order_totals;
use domain::{
    Address, Carrier, Country, Currency, Customer, CustomerId, Discount, Money, OrderStatus,
    Product, Sku,
};
use storage::{Store, decode, encode};

fn shop() -> Store {
    let mut store = Store::new();
    store
        .add_product(
            Product::new(
                Sku::parse("FUEL-1").unwrap(),
                "Camp Fuel",
                Money::new(900, Currency::Gbp),
                1500,
            )
            .with_hazard(true),
            2,
        )
        .unwrap();
    store
        .add_product(
            Product::new(
                Sku::parse("MUG-1").unwrap(),
                "Blue Mug",
                Money::new(1299, Currency::Gbp),
                400,
            ),
            10,
        )
        .unwrap();
    store
        .add_customer(Customer::new(
            CustomerId(2),
            "Bob",
            "bob@example.com",
            Address::new("1 High St", "London", "SW1A", Country::Gb),
        ))
        .unwrap();
    store
        .add_customer(Customer::new(
            CustomerId(3),
            "Yuki",
            "yuki@example.com",
            Address::new("2 Sakura", "Osaka", "530", Country::Other("JP".to_string())),
        ))
        .unwrap();
    store
}

#[test]
fn a_full_store_survives_a_round_trip() {
    let mut store = shop();
    let order = store
        .place_order(
            CustomerId(2),
            Discount::Percent { basis_points: 500 },
            &[
                (Sku::parse("FUEL-1").unwrap(), 1, Discount::None),
                (
                    Sku::parse("MUG-1").unwrap(),
                    2,
                    Discount::Percent { basis_points: 1000 },
                ),
            ],
        )
        .unwrap();
    store
        .transition(
            order,
            OrderStatus::Shipped {
                carrier: Carrier::Courier {
                    name: "ups".to_string(),
                    express: true,
                },
                tracking: "1Z9".to_string(),
            },
        )
        .unwrap();
    let second = store
        .place_order(
            CustomerId(3),
            Discount::FreeShipping,
            &[(Sku::parse("MUG-1").unwrap(), 1, Discount::None)],
        )
        .unwrap();
    store
        .transition(
            second,
            OrderStatus::Paid {
                reference: "PAY-9".to_string(),
                paid_cents: 1_299,
            },
        )
        .unwrap();

    let text = encode(&store);
    assert!(text.starts_with("# shopfront store v1\n"));
    let restored = decode(&text).unwrap();

    assert_eq!(encode(&restored), text);
    assert_eq!(restored.product_count(), 2);
    assert_eq!(restored.customer_count(), 2);
    assert_eq!(restored.order_count(), 2);
    assert_eq!(restored.total_stock(), store.total_stock());

    let shipped = restored.order(order).unwrap();
    assert_eq!(shipped.status, store.order(order).unwrap().status);
    assert_eq!(shipped.discount, store.order(order).unwrap().discount);
    assert_eq!(shipped.lines, store.order(order).unwrap().lines);
    assert_eq!(
        shipped.customer.billing_address,
        store.order(order).unwrap().customer.billing_address
    );
    assert_eq!(
        order_totals(shipped),
        order_totals(store.order(order).unwrap())
    );

    let paid = restored.order(second).unwrap();
    assert_eq!(paid.customer.loyalty_points, 12);
    assert_eq!(
        paid.status,
        OrderStatus::Paid {
            reference: "PAY-9".to_string(),
            paid_cents: 1299
        }
    );
    assert_eq!(restored.customer(CustomerId(3)).unwrap().name, "Yuki");
    assert_eq!(restored.next_order_id(), 1002);
}

#[test]
fn hazardous_flags_and_foreign_countries_are_kept() {
    let store = shop();
    let restored = decode(&encode(&store)).unwrap();
    let fuel = restored.product(&Sku::parse("FUEL-1").unwrap()).unwrap();
    assert!(fuel.hazardous);
    assert_eq!(fuel.weight_grams, 1500);
    assert_eq!(fuel.unit_price.format(), "9.00 GBP");
    let yuki = restored.customer(CustomerId(3)).unwrap();
    assert_eq!(
        yuki.billing_address.country,
        Country::Other("JP".to_string())
    );
}

#[test]
fn broken_snapshots_report_where_they_broke() {
    let cases = [
        (
            "product|MUG-1|Mug|1299|XXX|400|false|1",
            "snapshot line 1: unknown currency 'XXX'",
        ),
        (
            "customer|1|Ada|a@b|line|city",
            "snapshot line 1: missing postal code",
        ),
        ("shop|1", "snapshot line 1: unknown record 'shop'"),
        (
            "order|1000|9|EUR|placed|none|a|b|c|DE|",
            "unknown customer 9",
        ),
    ];
    for (source, expected) in cases {
        let error = decode(source).unwrap_err();
        assert_eq!(error.message(), expected);
    }
}

#[test]
fn an_empty_snapshot_is_an_empty_shop() {
    let restored = decode("# shopfront store v1\n\nmeta|1000\n").unwrap();
    assert_eq!(restored.product_count(), 0);
    assert_eq!(restored.order_count(), 0);
    assert_eq!(restored.next_order_id(), 1000);
    assert_eq!(encode(&restored), "# shopfront store v1\nmeta|1000\n");
}
