//! Integration coverage for the store: catalogue, stock, orders and queries.

use domain::{
    Address, Cents, Country, Currency, Customer, CustomerId, Discount, DomainError, Money, OrderId,
    OrderStatus, Product, Region, Sku,
};
use storage::{FIRST_ORDER_ID, StorageError, Store};

fn product(sku: &str, cents: Cents, weight: u32) -> Product {
    Product::new(
        Sku::parse(sku).unwrap(),
        format!("Article {sku}"),
        Money::new(cents, Currency::Eur),
        weight,
    )
}

fn customer(id: u64, country: Country) -> Customer {
    Customer::new(
        CustomerId(id),
        format!("Customer {id}"),
        format!("customer{id}@example.com"),
        Address::new("Hauptstr 1", "Berlin", "10115", country),
    )
}

fn shop() -> Store {
    let mut store = Store::new();
    store.add_product(product("MUG-1", 1299, 400), 10).unwrap();
    store.add_product(product("LAMP-2", 4000, 1200), 5).unwrap();
    store.add_customer(customer(1, Country::De)).unwrap();
    store.add_customer(customer(2, Country::Us)).unwrap();
    store
}

fn items(sku: &str, quantity: u32) -> Vec<(Sku, u32, Discount)> {
    vec![(Sku::parse(sku).unwrap(), quantity, Discount::None)]
}

#[test]
fn orders_are_numbered_and_grouped_by_region() {
    let mut store = shop();
    let first = store
        .place_order(CustomerId(1), Discount::None, &items("MUG-1", 2))
        .unwrap();
    let second = store
        .place_order(CustomerId(2), Discount::None, &items("LAMP-2", 1))
        .unwrap();
    assert_eq!(first, OrderId(FIRST_ORDER_ID));
    assert_eq!(second, OrderId(FIRST_ORDER_ID + 1));
    assert_eq!(store.order_count(), 2);
    assert_eq!(store.orders().len(), 2);
    assert_eq!(store.orders_for_customer(CustomerId(1)).len(), 1);
    assert_eq!(store.orders_for_customer(CustomerId(2)).len(), 1);
    assert_eq!(store.orders_for_customer(CustomerId(9)).len(), 0);

    let revenue = store.revenue_by_region();
    assert_eq!(revenue.len(), 2);
    assert_eq!(revenue[0].region, Region::Europe);
    assert_eq!(revenue[0].orders, 1);
    assert_eq!(revenue[0].total_cents, 4_411);
    assert_eq!(revenue[1].region, Region::NorthAmerica);
    assert_eq!(revenue[1].total_cents, 5_779);
    assert_eq!(revenue[1].currency, Currency::Eur);
}

#[test]
fn order_lines_freeze_the_catalogue_data() {
    let mut store = shop();
    let id = store
        .place_order(CustomerId(1), Discount::None, &items("MUG-1", 3))
        .unwrap();
    let order = store.order(id).unwrap();
    let line = &order.lines[0];
    assert_eq!(line.sku.as_str(), "MUG-1");
    assert_eq!(line.quantity, 3);
    assert_eq!(line.unit_price, Money::new(1299, Currency::Eur));
    assert_eq!(line.weight_grams, 400);
    assert!(!line.hazardous);
    assert_eq!(order.total_weight(), 1200);
    assert_eq!(order.currency, Currency::Eur);
    assert_eq!(
        order.ship_to,
        store.customer(CustomerId(1)).unwrap().billing_address
    );
    // Stock moved, the catalogue is untouched.
    assert_eq!(store.stock_of(&Sku::parse("MUG-1").unwrap()), 7);
    assert_eq!(
        store
            .product(&Sku::parse("MUG-1").unwrap())
            .unwrap()
            .unit_price
            .cents,
        1299
    );
}

#[test]
fn reservations_never_oversell() {
    let mut store = shop();
    let sku = Sku::parse("LAMP-2").unwrap();
    let held = store.reserve(&sku, 4).unwrap();
    assert_eq!(store.available(&sku), 1);
    assert_eq!(
        store.reserve(&sku, 2),
        Err(StorageError::InsufficientStock {
            sku: "LAMP-2".to_string(),
            requested: 2,
            available: 1,
        })
    );
    assert_eq!(
        store.place_order(CustomerId(1), Discount::None, &items("LAMP-2", 2)),
        Err(StorageError::InsufficientStock {
            sku: "LAMP-2".to_string(),
            requested: 2,
            available: 1,
        })
    );
    store.commit(held);
    assert_eq!(store.stock_of(&sku), 1);
    assert_eq!(store.available(&sku), 1);
    assert_eq!(store.total_stock(), 11);

    let another = store.reserve(&sku, 1).unwrap();
    store.release(another);
    assert_eq!(store.stock_of(&sku), 1);
    assert_eq!(store.reserved_of(&sku), 0);
}

#[test]
fn a_failed_basket_reserves_nothing() {
    let mut store = shop();
    let basket = vec![
        (Sku::parse("MUG-1").unwrap(), 1, Discount::None),
        (Sku::parse("LAMP-2").unwrap(), 9, Discount::None),
    ];
    assert!(matches!(
        store.place_order(CustomerId(1), Discount::None, &basket),
        Err(StorageError::InsufficientStock { .. })
    ));
    let sku = Sku::parse("MUG-1").unwrap();
    assert_eq!(store.available(&sku), 10);
    assert_eq!(store.reserved_of(&sku), 0);
    assert_eq!(store.order_count(), 0);
}

#[test]
fn payments_must_cover_the_total() {
    let mut store = shop();
    let id = store
        .place_order(CustomerId(1), Discount::None, &items("MUG-1", 2))
        .unwrap();
    assert_eq!(
        store.transition(
            id,
            OrderStatus::Paid {
                reference: "PAY-1".to_string(),
                paid_cents: 1,
            }
        ),
        Err(StorageError::Underpaid {
            due: 4_411,
            paid: 1,
        })
    );
    let paid = store
        .transition(
            id,
            OrderStatus::Paid {
                reference: "PAY-1".to_string(),
                paid_cents: 4_411,
            },
        )
        .unwrap();
    assert_eq!(paid.label(), "Paid");
    assert_eq!(store.open_orders().len(), 1);
    assert_eq!(store.customer(CustomerId(1)).unwrap().loyalty_points, 44);
    store
        .transition(
            id,
            OrderStatus::Shipped {
                carrier: domain::Carrier::Postal,
                tracking: "TRACK-1".to_string(),
            },
        )
        .unwrap();
    assert_eq!(store.open_orders().len(), 0);
    assert_eq!(
        store.transition(
            OrderId(4242),
            OrderStatus::Cancelled {
                reason: domain::CancelReason::CustomerRequest
            }
        ),
        Err(StorageError::UnknownOrder(4242))
    );
    assert_eq!(
        store.transition(
            id,
            OrderStatus::Shipped {
                carrier: domain::Carrier::Pickup,
                tracking: " ".to_string(),
            }
        ),
        Err(StorageError::InvalidText("tracking"))
    );
}

#[test]
fn catalogue_rejects_bad_values() {
    let mut store = shop();
    assert_eq!(
        store.add_product(product("MUG-1", 1, 1), 1),
        Err(StorageError::DuplicateSku("MUG-1".to_string()))
    );
    assert_eq!(
        store.add_customer(customer(1, Country::De)),
        Err(StorageError::DuplicateCustomer(1))
    );
    let broken = Product::new(
        Sku::parse("BAD-1").unwrap(),
        "name;with;separators",
        Money::new(1, Currency::Eur),
        1,
    );
    assert_eq!(
        store.add_product(broken, 1),
        Err(StorageError::InvalidText("product name"))
    );
    assert_eq!(
        store.place_order(CustomerId(7), Discount::None, &items("MUG-1", 1)),
        Err(StorageError::UnknownCustomer(7))
    );
    assert_eq!(
        store.place_order(CustomerId(1), Discount::None, &items("NOPE-1", 1)),
        Err(StorageError::UnknownSku("NOPE-1".to_string()))
    );
    assert!(matches!(
        store.place_order(
            CustomerId(1),
            Discount::None,
            &[(Sku::parse("MUG-1").unwrap(), 0, Discount::None)]
        ),
        Err(StorageError::Domain(DomainError::InvalidQuantity(0)))
    ));
}

#[test]
fn low_stock_lists_only_what_is_running_out() {
    let mut store = shop();
    store
        .place_order(CustomerId(1), Discount::None, &items("LAMP-2", 4))
        .unwrap();
    let low = store.low_stock(1);
    assert_eq!(low.len(), 1);
    assert_eq!(low[0].0.sku.as_str(), "LAMP-2");
    assert_eq!(low[0].1, 1);
    assert_eq!(store.low_stock(10).len(), 2);
    assert_eq!(store.products()[0].sku.as_str(), "LAMP-2");
}
