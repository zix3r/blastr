//! The store itself: hashmaps behind a small, intention revealing API.

use std::collections::HashMap;

use billing::order_totals;
use domain::{
    Cents, Currency, Customer, CustomerId, Discount, DomainError, Order, OrderId, OrderLine,
    OrderStatus, Product, Region, Sku,
};

use crate::error::StorageError;
use crate::text;

/// The id of the first order a fresh store hands out.
pub const FIRST_ORDER_ID: u64 = 1000;

/// A claim on stock that has not been committed or released yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub sku: Sku,
    pub quantity: u32,
}

/// The orders of one taxing region and what they are worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionRevenue {
    pub region: Region,
    pub orders: usize,
    pub total_cents: Cents,
    pub currency: Currency,
}

/// Everything a shop needs to answer a request.
#[derive(Clone, Debug)]
pub struct Store {
    pub(crate) products: HashMap<Sku, Product>,
    pub(crate) inventory: HashMap<Sku, u32>,
    pub(crate) reserved: HashMap<Sku, u32>,
    pub(crate) customers: HashMap<CustomerId, Customer>,
    pub(crate) orders: HashMap<OrderId, Order>,
    pub(crate) next_order_id: u64,
}

impl Default for Store {
    fn default() -> Store {
        Store::new()
    }
}

impl Store {
    /// An empty shop that starts numbering orders at [`FIRST_ORDER_ID`].
    pub fn new() -> Store {
        Store {
            products: HashMap::new(),
            inventory: HashMap::new(),
            reserved: HashMap::new(),
            customers: HashMap::new(),
            orders: HashMap::new(),
            next_order_id: FIRST_ORDER_ID,
        }
    }

    /// Adds an article with its opening stock.
    pub fn add_product(&mut self, product: Product, stock: u32) -> Result<(), StorageError> {
        text::check_text("product name", &product.name)?;
        if self.products.contains_key(&product.sku) {
            return Err(StorageError::DuplicateSku(product.sku.to_string()));
        }
        self.inventory.insert(product.sku.clone(), stock);
        self.products.insert(product.sku.clone(), product);
        Ok(())
    }

    /// Registers a customer.
    pub fn add_customer(&mut self, customer: Customer) -> Result<(), StorageError> {
        text::check_text("customer name", &customer.name)?;
        text::check_text("email", &customer.email)?;
        text::check_text("address line", &customer.billing_address.line1)?;
        text::check_text("city", &customer.billing_address.city)?;
        if self.customers.contains_key(&customer.id) {
            return Err(StorageError::DuplicateCustomer(customer.id.0));
        }
        self.customers.insert(customer.id, customer);
        Ok(())
    }

    /// The product behind `sku`.
    pub fn product(&self, sku: &Sku) -> Option<&Product> {
        self.products.get(sku)
    }

    /// The customer behind `id`.
    pub fn customer(&self, id: CustomerId) -> Option<&Customer> {
        self.customers.get(&id)
    }

    /// The order behind `id`.
    pub fn order(&self, id: OrderId) -> Option<&Order> {
        self.orders.get(&id)
    }

    /// Physical stock of `sku`, ignoring reservations.
    pub fn stock_of(&self, sku: &Sku) -> u32 {
        self.inventory.get(sku).copied().unwrap_or(0)
    }

    /// Units of `sku` currently held by reservations.
    pub fn reserved_of(&self, sku: &Sku) -> u32 {
        self.reserved.get(sku).copied().unwrap_or(0)
    }

    /// Units that can still be promised.
    pub fn available(&self, sku: &Sku) -> u32 {
        self.stock_of(sku).saturating_sub(self.reserved_of(sku))
    }

    /// Number of articles in the catalogue.
    pub fn product_count(&self) -> usize {
        self.products.len()
    }

    /// Number of registered customers.
    pub fn customer_count(&self) -> usize {
        self.customers.len()
    }

    /// Number of stored orders.
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }

    /// The id the next order will get.
    pub fn next_order_id(&self) -> u64 {
        self.next_order_id
    }

    /// Total physical stock across the catalogue.
    pub fn total_stock(&self) -> u32 {
        self.inventory
            .values()
            .fold(0_u32, |total, stock| total.saturating_add(*stock))
    }

    /// Every product, ordered by SKU.
    pub fn products(&self) -> Vec<&Product> {
        let mut products: Vec<&Product> = self.products.values().collect();
        products.sort_by(|left, right| left.sku.cmp(&right.sku));
        products
    }

    /// Every order, ordered by id.
    pub fn orders(&self) -> Vec<&Order> {
        let mut orders: Vec<&Order> = self.orders.values().collect();
        orders.sort_by_key(|order| order.id);
        orders
    }

    /// The orders of one customer, ordered by id.
    pub fn orders_for_customer(&self, id: CustomerId) -> Vec<&Order> {
        let mut orders: Vec<&Order> = self
            .orders
            .values()
            .filter(|order| order.customer.id == id)
            .collect();
        orders.sort_by_key(|order| order.id);
        orders
    }

    /// Orders that still need warehouse work.
    pub fn open_orders(&self) -> Vec<&Order> {
        let mut orders: Vec<&Order> = self
            .orders
            .values()
            .filter(|order| order.status.is_open())
            .collect();
        orders.sort_by_key(|order| order.id);
        orders
    }

    /// Products whose physical stock is at or below `threshold`, ordered by SKU.
    pub fn low_stock(&self, threshold: u32) -> Vec<(&Product, u32)> {
        let mut items: Vec<(&Product, u32)> = self
            .products()
            .into_iter()
            .map(|product| (product, self.stock_of(&product.sku)))
            .filter(|(_, stock)| *stock <= threshold)
            .collect();
        items.sort_by(|left, right| left.0.sku.cmp(&right.0.sku));
        items
    }

    /// Grand totals per taxing region, ordered by region.
    pub fn revenue_by_region(&self) -> Vec<RegionRevenue> {
        let mut rows: Vec<RegionRevenue> = Vec::new();
        for order in self.orders() {
            let region = order.customer.region();
            let total = order_totals(order).grand_total;
            match rows.iter_mut().find(|row| row.region == region) {
                Some(row) => {
                    row.orders += 1;
                    row.total_cents = row.total_cents.saturating_add(total);
                }
                None => rows.push(RegionRevenue {
                    region,
                    orders: 1,
                    total_cents: total,
                    currency: order.currency,
                }),
            }
        }
        rows.sort_by_key(|row| row.region);
        rows
    }

    /// Holds `quantity` units of `sku` for a caller that is still building an
    /// order. The reservation must be committed or released.
    pub fn reserve(&mut self, sku: &Sku, quantity: u32) -> Result<Reservation, StorageError> {
        if quantity == 0 {
            return Err(StorageError::Domain(DomainError::InvalidQuantity(quantity)));
        }
        if !self.products.contains_key(sku) {
            return Err(StorageError::UnknownSku(sku.to_string()));
        }
        let available = self.available(sku);
        if available < quantity {
            return Err(StorageError::InsufficientStock {
                sku: sku.to_string(),
                requested: quantity,
                available,
            });
        }
        let held = self.reserved.entry(sku.clone()).or_insert(0);
        *held = held.saturating_add(quantity);
        Ok(Reservation {
            sku: sku.clone(),
            quantity,
        })
    }

    /// Drops a reservation without touching physical stock.
    pub fn release(&mut self, reservation: Reservation) {
        if let Some(held) = self.reserved.get_mut(&reservation.sku) {
            *held = held.saturating_sub(reservation.quantity);
        }
    }

    /// Turns a reservation into shipped-from-warehouse stock.
    pub fn commit(&mut self, reservation: Reservation) {
        if let Some(stock) = self.inventory.get_mut(&reservation.sku) {
            *stock = stock.saturating_sub(reservation.quantity);
        }
        self.release(reservation);
    }

    /// Checks the baskets the store accepts before anything is reserved.
    ///
    /// Returns the currency every line is priced in.
    pub(crate) fn validate_items(
        &self,
        items: &[(Sku, u32, Discount)],
    ) -> Result<Currency, StorageError> {
        let mut currency: Option<Currency> = None;
        for (sku, quantity, _) in items {
            let product = self
                .products
                .get(sku)
                .ok_or_else(|| StorageError::UnknownSku(sku.to_string()))?;
            if *quantity == 0 {
                return Err(StorageError::Domain(DomainError::InvalidQuantity(
                    *quantity,
                )));
            }
            match currency {
                None => currency = Some(product.unit_price.currency),
                Some(current) if current != product.unit_price.currency => {
                    return Err(StorageError::Domain(DomainError::CurrencyMismatch {
                        left: current,
                        right: product.unit_price.currency,
                    }));
                }
                Some(_) => {}
            }
        }
        currency.ok_or(StorageError::EmptyOrder)
    }

    /// Prices a basket, reserves the stock, stores the order and returns its id.
    pub fn place_order(
        &mut self,
        customer_id: CustomerId,
        discount: Discount,
        items: &[(Sku, u32, Discount)],
    ) -> Result<OrderId, StorageError> {
        let currency = self.validate_items(items)?;
        let customer = self
            .customers
            .get(&customer_id)
            .cloned()
            .ok_or(StorageError::UnknownCustomer(customer_id.0))?;

        let mut reservations = Vec::with_capacity(items.len());
        for (sku, quantity, _) in items {
            match self.reserve(sku, *quantity) {
                Ok(reservation) => reservations.push(reservation),
                Err(error) => {
                    for held in reservations {
                        self.release(held);
                    }
                    return Err(error);
                }
            }
        }

        let ship_to = customer.billing_address.clone();
        let mut order = Order::new(OrderId(self.next_order_id), customer, ship_to, currency);
        for (sku, quantity, line_discount) in items {
            // `validate_items` already proved the product exists and that every
            // line is priced in the order currency.
            let product = self
                .products
                .get(sku)
                .ok_or_else(|| StorageError::UnknownSku(sku.to_string()))?;
            let line = OrderLine {
                sku: sku.clone(),
                quantity: *quantity,
                unit_price: product.unit_price,
                weight_grams: product.weight_grams,
                hazardous: product.hazardous,
                discount: line_discount.clone(),
            };
            order.add_line(line)?;
        }
        order.discount = discount;
        order.status = OrderStatus::Placed;

        for reservation in reservations {
            self.commit(reservation);
        }
        let id = order.id;
        self.orders.insert(id, order);
        self.next_order_id += 1;
        Ok(id)
    }

    /// Moves an order to `next`, checking that the payment covers the total.
    ///
    /// Paid orders credit one loyalty point per whole currency unit to the
    /// customer record.
    pub fn transition(
        &mut self,
        id: OrderId,
        next: OrderStatus,
    ) -> Result<OrderStatus, StorageError> {
        let order = self
            .orders
            .get_mut(&id)
            .ok_or(StorageError::UnknownOrder(id.0))?;
        let total = order_totals(order).grand_total;
        match &next {
            OrderStatus::Paid { paid_cents, .. } => {
                if *paid_cents < total {
                    return Err(StorageError::Underpaid {
                        due: total,
                        paid: *paid_cents,
                    });
                }
            }
            OrderStatus::Shipped { tracking, .. } if tracking.trim().is_empty() => {
                return Err(StorageError::InvalidText("tracking"));
            }
            _ => {}
        }
        order.status = next.clone();
        if matches!(next, OrderStatus::Paid { .. }) {
            order.customer.add_loyalty(total);
            if let Some(record) = self.customers.get_mut(&order.customer.id) {
                record.loyalty_points = order.customer.loyalty_points;
            }
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Address, Country, Money};

    fn customer() -> Customer {
        Customer::new(
            CustomerId(1),
            "Ada",
            "ada@example.com",
            Address::new("Hauptstr 1", "Berlin", "10115", Country::De),
        )
    }

    fn product(sku: &str, cents: Cents) -> Product {
        Product::new(
            Sku::parse(sku).unwrap(),
            "Widget",
            Money::new(cents, Currency::Eur),
            400,
        )
    }

    fn store() -> Store {
        let mut store = Store::new();
        store.add_product(product("MUG-1", 1299), 5).unwrap();
        store.add_customer(customer()).unwrap();
        store
    }

    fn mug(qty: u32) -> (Sku, u32, Discount) {
        (Sku::parse("MUG-1").unwrap(), qty, Discount::None)
    }

    #[test]
    fn products_are_unique_and_counted() {
        let mut store = store();
        assert_eq!(store.product_count(), 1);
        assert_eq!(store.stock_of(&Sku::parse("MUG-1").unwrap()), 5);
        assert_eq!(store.total_stock(), 5);
        assert_eq!(
            store.add_product(product("MUG-1", 100), 1),
            Err(StorageError::DuplicateSku("MUG-1".to_string()))
        );
        assert_eq!(
            store.add_customer(customer()),
            Err(StorageError::DuplicateCustomer(1))
        );
        assert_eq!(
            store.add_product(
                Product::new(
                    Sku::parse("BAD-1").unwrap(),
                    "pipe|name",
                    Money::new(1, Currency::Eur),
                    1
                ),
                1
            ),
            Err(StorageError::InvalidText("product name"))
        );
    }

    #[test]
    fn reservations_hold_and_release_stock() {
        let mut store = store();
        let sku = Sku::parse("MUG-1").unwrap();
        let first = store.reserve(&sku, 3).unwrap();
        assert_eq!(store.available(&sku), 2);
        assert_eq!(store.stock_of(&sku), 5);
        let second = store.reserve(&sku, 3).unwrap_err();
        assert_eq!(
            second,
            StorageError::InsufficientStock {
                sku: "MUG-1".to_string(),
                requested: 3,
                available: 2,
            }
        );
        store.commit(first);
        assert_eq!(store.stock_of(&sku), 2);
        assert_eq!(store.available(&sku), 2);
        assert_eq!(store.reserved_of(&sku), 0);
    }

    #[test]
    fn releasing_a_reservation_leaves_stock_alone() {
        let mut store = store();
        let sku = Sku::parse("MUG-1").unwrap();
        let reservation = store.reserve(&sku, 4).unwrap();
        store.release(reservation);
        assert_eq!(store.stock_of(&sku), 5);
        assert_eq!(store.reserved_of(&sku), 0);
        assert!(matches!(
            store.reserve(&sku, 0),
            Err(StorageError::Domain(DomainError::InvalidQuantity(0)))
        ));
        assert_eq!(
            store.reserve(&Sku::parse("NOPE-1").unwrap(), 1),
            Err(StorageError::UnknownSku("NOPE-1".to_string()))
        );
    }

    #[test]
    fn placing_an_order_moves_stock_and_numbers_orders() {
        let mut store = store();
        let id = store
            .place_order(CustomerId(1), Discount::None, &[mug(2)])
            .unwrap();
        assert_eq!(id, OrderId(FIRST_ORDER_ID));
        assert_eq!(store.order_count(), 1);
        assert_eq!(store.stock_of(&Sku::parse("MUG-1").unwrap()), 3);
        let order = store.order(id).unwrap();
        assert_eq!(order.status, OrderStatus::Placed);
        assert_eq!(order.lines.len(), 1);
        assert_eq!(order.lines[0].quantity, 2);
        assert_eq!(store.orders_for_customer(CustomerId(1)).len(), 1);
        assert_eq!(store.open_orders().len(), 1);
    }

    #[test]
    fn orders_are_rejected_without_stock_or_customer() {
        let mut store = store();
        assert!(matches!(
            store.place_order(CustomerId(1), Discount::None, &[mug(9)]),
            Err(StorageError::InsufficientStock { .. })
        ));
        assert_eq!(
            store.place_order(CustomerId(2), Discount::None, &[mug(1)]),
            Err(StorageError::UnknownCustomer(2))
        );
        assert_eq!(
            store.place_order(CustomerId(1), Discount::None, &[]),
            Err(StorageError::EmptyOrder)
        );
        // Nothing was reserved by the failed attempts.
        assert_eq!(store.stock_of(&Sku::parse("MUG-1").unwrap()), 5);
        assert_eq!(store.order_count(), 0);
    }

    #[test]
    fn low_stock_and_revenue_are_sorted() {
        let mut store = store();
        store.add_product(product("AAA-1", 500), 1).unwrap();
        let id = store
            .place_order(CustomerId(1), Discount::None, &[mug(2)])
            .unwrap();
        assert_eq!(
            store
                .low_stock(3)
                .iter()
                .map(|(product, stock)| (product.sku.to_string(), *stock))
                .collect::<Vec<_>>(),
            vec![("AAA-1".to_string(), 1), ("MUG-1".to_string(), 3)]
        );
        assert_eq!(store.products().len(), 2);
        let revenue = store.revenue_by_region();
        assert_eq!(revenue.len(), 1);
        assert_eq!(revenue[0].region, Region::Europe);
        assert_eq!(revenue[0].orders, 1);
        assert_eq!(revenue[0].currency, Currency::Eur);
        assert!(revenue[0].total_cents > 0);
        assert_eq!(store.order(id).unwrap().id, id);
        assert_eq!(store.customer_count(), 1);
        assert_eq!(store.customer(CustomerId(1)).unwrap().name, "Ada");
    }
}
