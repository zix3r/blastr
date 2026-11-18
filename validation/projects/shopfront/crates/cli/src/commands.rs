//! The command layer: a [`Session`] owns a store plus the output it produced.

use billing::{generate_invoice, invoice_summary, order_totals};
use domain::{
    Address, CancelReason, Carrier, Cents, Country, Currency, Customer, CustomerId, Discount,
    Money, OrderId, OrderStatus, Product, Sku,
};
use storage::Store;

use crate::command::{Command, CommandError};
use crate::format;

/// Stock at or below which the report calls an article low.
pub const LOW_STOCK_THRESHOLD: u32 = 5;

/// One shop session: the store, the lines printed so far and the error count.
#[derive(Debug)]
pub struct Session {
    store: Store,
    out: Vec<String>,
    errors: usize,
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

impl Session {
    /// An empty shop.
    pub fn new() -> Session {
        Session {
            store: Store::new(),
            out: Vec::new(),
            errors: 0,
        }
    }

    /// The store behind the session, for tests and embedders.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// How many lines failed.
    pub fn errors(&self) -> usize {
        self.errors
    }

    /// The output produced so far.
    pub fn output(&self) -> &[String] {
        &self.out
    }

    /// Runs every line of `script`, recording failures instead of stopping.
    pub fn run(&mut self, script: &str) {
        for line in script.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Err(error) = self.execute(trimmed) {
                self.errors += 1;
                self.out.push(format!("error: {error}"));
            }
        }
    }

    /// The output as one newline terminated block.
    pub fn into_output(self) -> String {
        if self.out.is_empty() {
            return String::new();
        }
        let mut text = self.out.join("\n");
        text.push('\n');
        text
    }

    /// Runs a single command line.
    pub fn execute(&mut self, line: &str) -> Result<(), CommandError> {
        let command = Command::parse(line)?;
        match command.name() {
            "add-product" => self.add_product(&command),
            "add-customer" => self.add_customer(&command),
            "place-order" => self.place_order(&command),
            "advance" => self.advance(&command),
            "invoice" => self.invoice(&command),
            "report" => self.report(&command),
            other => Err(CommandError::new(format!("unknown command '{other}'"))),
        }
    }

    fn add_product(&mut self, command: &Command) -> Result<(), CommandError> {
        let sku = Sku::parse(command.required("sku")?)?;
        let currency = Currency::from_code(command.required("currency")?)?;
        let price = command.cents("price")?;
        let weight = command.count("weight")?;
        let stock = command.count("stock")?;
        let product = Product::new(
            sku.clone(),
            command.required("name")?,
            Money::new(price, currency),
            weight,
        )
        .with_hazard(command.flag("hazard"));
        self.store.add_product(product, stock)?;
        let stored = self
            .store
            .product(&sku)
            .ok_or_else(|| CommandError::new(format!("product {sku} vanished")))?;
        let line = format::product_line(stored, self.store.stock_of(&sku));
        self.out.push(line);
        Ok(())
    }

    fn add_customer(&mut self, command: &Command) -> Result<(), CommandError> {
        let id = CustomerId(command.number("id")?);
        let address = Address {
            line1: command.required("line1")?.to_string(),
            city: command.required("city")?.to_string(),
            postal_code: command.required("postal")?.to_string(),
            country: Country::from_code(command.required("country")?)?,
        };
        let customer = Customer::new(
            id,
            command.required("name")?,
            command.required("email")?,
            address,
        );
        self.store.add_customer(customer.clone())?;
        self.out.push(format::customer_line(&customer));
        Ok(())
    }

    fn place_order(&mut self, command: &Command) -> Result<(), CommandError> {
        let customer_id = CustomerId(command.number("customer")?);
        let items_spec = command.required("items")?;
        let currency = self.basket_currency(items_spec)?;
        let items = parse_items(items_spec, currency)?;
        let discount = match command.raw("discount") {
            Some(spec) => Discount::parse(spec, currency)?,
            None => Discount::None,
        };
        let id = self.store.place_order(customer_id, discount, &items)?;
        let totals = self.totals_for(id)?;
        let line = invoice_summary(id, &totals);
        self.out.push(line);
        Ok(())
    }

    fn advance(&mut self, command: &Command) -> Result<(), CommandError> {
        let id = OrderId(command.number("order")?);
        let order = self
            .store
            .order(id)
            .ok_or_else(|| CommandError::new(format!("unknown order #{id}")))?;
        let current = order.status.clone();
        let currency = order.currency;
        let total = order_totals(order).grand_total;
        let target = command.required("to")?;
        let next = build_status(target, command, total, currency)?;
        if !transition_allowed(&current, &next) {
            return Err(CommandError::new(format!(
                "cannot go from {} to {}",
                current.label(),
                next.label()
            )));
        }
        let status = self.store.transition(id, next)?;
        self.out.push(format::status_line(id, &status, currency));
        Ok(())
    }

    fn invoice(&mut self, command: &Command) -> Result<(), CommandError> {
        let id = OrderId(command.number("order")?);
        let order = self
            .store
            .order(id)
            .ok_or_else(|| CommandError::new(format!("unknown order #{id}")))?;
        let totals = order_totals(order);
        let text = generate_invoice(order, &totals);
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        self.out.extend(lines);
        Ok(())
    }

    fn report(&mut self, command: &Command) -> Result<(), CommandError> {
        let threshold = match command.raw("threshold") {
            Some(text) => parse_count(text)?,
            None => LOW_STOCK_THRESHOLD,
        };
        self.out.push(format::report_header(
            self.store.product_count(),
            self.store.customer_count(),
            self.store.order_count(),
        ));
        for row in self.store.revenue_by_region() {
            self.out.push(format::revenue_line(&row));
        }
        for (product, stock) in self.store.low_stock(threshold) {
            self.out.push(format::low_stock_line(product, stock));
        }
        Ok(())
    }

    /// The currency of a basket, taken from its first article.
    fn basket_currency(&self, items_spec: &str) -> Result<Currency, CommandError> {
        let first = items_spec.split(',').next().unwrap_or_default();
        let sku = Sku::parse(first.split(':').next().unwrap_or_default())?;
        self.store
            .product(&sku)
            .map(|product| product.unit_price.currency)
            .ok_or_else(|| CommandError::new(format!("unknown product '{sku}'")))
    }

    fn totals_for(&self, id: OrderId) -> Result<billing::Totals, CommandError> {
        let order = self
            .store
            .order(id)
            .ok_or_else(|| CommandError::new(format!("unknown order #{id}")))?;
        Ok(order_totals(order))
    }
}

/// Parses `SKU:qty[:discount]` groups separated by commas.
pub fn parse_items(
    spec: &str,
    currency: Currency,
) -> Result<Vec<(Sku, u32, Discount)>, CommandError> {
    let mut items = Vec::new();
    for group in spec.split(',') {
        if group.trim().is_empty() {
            return Err(CommandError::new("empty basket item"));
        }
        let mut parts = group.split(':');
        let sku = Sku::parse(parts.next().unwrap_or_default())?;
        let quantity_text = parts
            .next()
            .ok_or_else(|| CommandError::new(format!("'{group}' needs a quantity")))?;
        let quantity = parse_count(quantity_text)?;
        if quantity == 0 || quantity > billing::MAX_LINE_QUANTITY {
            return Err(CommandError::new(format!(
                "quantity must be between 1 and {}",
                billing::MAX_LINE_QUANTITY
            )));
        }
        let rest: Vec<&str> = parts.collect();
        let discount = if rest.is_empty() {
            Discount::None
        } else {
            Discount::parse(&rest.join(":"), currency)?
        };
        items.push((sku, quantity, discount));
    }
    Ok(items)
}

/// Whether `next` is a legal step from `current`.
///
/// Terminal states can only be left by the refund step, and nothing moves
/// backwards in the lifecycle.
fn transition_allowed(current: &OrderStatus, next: &OrderStatus) -> bool {
    match (current, next) {
        (OrderStatus::Draft, OrderStatus::Placed) => true,
        (OrderStatus::Placed, OrderStatus::Paid { .. }) => true,
        (OrderStatus::Paid { .. }, OrderStatus::Shipped { .. }) => true,
        (OrderStatus::Shipped { .. }, OrderStatus::Delivered { .. }) => true,
        (OrderStatus::Placed | OrderStatus::Paid { .. }, OrderStatus::Cancelled { .. }) => true,
        (OrderStatus::Delivered { .. }, OrderStatus::Refunded { .. }) => true,
        (OrderStatus::Draft | OrderStatus::Placed | OrderStatus::Paid { .. }, _) => false,
        (OrderStatus::Shipped { .. } | OrderStatus::Delivered { .. }, _) => false,
        (OrderStatus::Cancelled { .. } | OrderStatus::Refunded { .. }, _) => false,
    }
}

fn build_status(
    target: &str,
    command: &Command,
    total: Cents,
    currency: Currency,
) -> Result<OrderStatus, CommandError> {
    match target {
        "paid" => {
            let reference = command.required("ref")?.to_string();
            let paid_cents = match command.raw("amount") {
                Some(text) => parse_cents(text)?,
                None => total,
            };
            Ok(OrderStatus::Paid {
                reference,
                paid_cents,
            })
        }
        "shipped" => {
            let carrier = Carrier::parse(command.raw("carrier").unwrap_or("postal"))?;
            let tracking = command.required("tracking")?.to_string();
            Ok(OrderStatus::Shipped { carrier, tracking })
        }
        "delivered" => {
            let at_day = match command.raw("day") {
                Some(text) => parse_count(text)?,
                None => 1,
            };
            Ok(OrderStatus::Delivered { at_day })
        }
        "cancelled" => Ok(OrderStatus::Cancelled {
            reason: CancelReason::parse(command.raw("reason").unwrap_or("request"))?,
        }),
        "refunded" => {
            let amount = match command.raw("amount") {
                Some(text) => parse_cents(text)?,
                None => total,
            };
            Ok(OrderStatus::Refunded {
                amount: Money::new(amount, currency),
                reason: CancelReason::parse(command.raw("reason").unwrap_or("request"))?,
            })
        }
        other => Err(CommandError::new(format!(
            "unknown target status '{other}'"
        ))),
    }
}

fn parse_count(text: &str) -> Result<u32, CommandError> {
    text.parse()
        .map_err(|_| CommandError::new(format!("'{text}' is not a count")))
}

fn parse_cents(text: &str) -> Result<Cents, CommandError> {
    let cents: Cents = text
        .parse()
        .map_err(|_| CommandError::new(format!("'{text}' is not an amount")))?;
    if cents < 0 {
        return Err(CommandError::new("amounts must not be negative"));
    }
    Ok(cents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Region;
    use storage::FIRST_ORDER_ID;

    fn shop() -> Session {
        let mut session = Session::new();
        session
            .run(
                "add-product sku=MUG-1 name=\"Blue Mug\" price=1299 currency=EUR weight=400 stock=25\n\
                 add-customer id=1 name=\"Ada\" email=ada@example.com line1=\"Hauptstr 1\" city=Berlin postal=10115 country=DE",
            );
        assert_eq!(session.errors(), 0);
        session
    }

    #[test]
    fn items_parse_with_and_without_reductions() {
        let items = parse_items("MUG-1:2,LAMP-2:1:percent:10", Currency::Eur).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0.as_str(), "MUG-1");
        assert_eq!(items[0].1, 2);
        assert_eq!(items[0].2, Discount::None);
        assert_eq!(items[1].2, Discount::Percent { basis_points: 1000 });

        assert!(parse_items("", Currency::Eur).is_err());
        assert!(parse_items("MUG-1", Currency::Eur).is_err());
        assert!(parse_items("MUG-1:0", Currency::Eur).is_err());
        assert!(parse_items("MUG-1:100", Currency::Eur).is_err());
        assert!(parse_items("MUG-1:2:percent:0", Currency::Eur).is_err());
    }

    #[test]
    fn a_whole_purchase_flow_runs_through_the_session() {
        let mut session = shop();
        session
            .execute("place-order customer=1 items=MUG-1:2")
            .unwrap();
        let id = OrderId(FIRST_ORDER_ID);
        assert_eq!(session.store().order_count(), 1);
        assert_eq!(
            session.store().order(id).unwrap().status,
            OrderStatus::Placed
        );
        session
            .execute("advance order=1000 to=paid ref=PAY-1")
            .unwrap();
        assert_eq!(
            session.store().order(id).unwrap().status,
            OrderStatus::Paid {
                reference: "PAY-1".to_string(),
                paid_cents: 4411
            }
        );
        assert_eq!(
            session
                .store()
                .customer(CustomerId(1))
                .unwrap()
                .loyalty_points,
            44
        );
        assert_eq!(session.output().len(), 4);
        assert_eq!(
            session.output()[3],
            "order #1000 status=paid (paid 44.11 EUR via PAY-1)"
        );
    }

    #[test]
    fn transitions_are_guarded() {
        let mut session = shop();
        session
            .execute("place-order customer=1 items=MUG-1:1")
            .unwrap();
        let error = session
            .execute("advance order=1000 to=delivered")
            .unwrap_err();
        assert_eq!(error.message(), "cannot go from Placed to Delivered");
        let error = session
            .execute("advance order=1000 to=paid ref=X amount=1")
            .unwrap_err();
        assert!(error.message().contains("does not cover"));
        let error = session.execute("advance order=1000 to=warp").unwrap_err();
        assert_eq!(error.message(), "unknown target status 'warp'");
        session
            .execute("advance order=1000 to=cancelled reason=out-of-stock:MUG-1")
            .unwrap();
        assert_eq!(
            session.store().order(OrderId(1000)).unwrap().status,
            OrderStatus::Cancelled {
                reason: CancelReason::OutOfStock {
                    sku: "MUG-1".to_string()
                }
            }
        );
    }

    #[test]
    fn reports_group_by_region() {
        let mut session = shop();
        session
            .execute("place-order customer=1 items=MUG-1:3")
            .unwrap();
        session.execute("report").unwrap();
        let header = &session.output()[3];
        assert_eq!(header, "report products=1 customers=1 orders=1");
        let rows = session.store().revenue_by_region();
        assert_eq!(rows[0].region, Region::Europe);
        assert!(rows[0].total_cents > 3897);
    }

    #[test]
    fn bad_lines_are_reported_and_do_not_stop_the_script() {
        let mut session = Session::new();
        session.run(
            "add-product sku=nope name=x price=1 currency=EUR weight=1 stock=1\n\
             report\n\
             frobnicate",
        );
        assert_eq!(session.errors(), 2);
        assert_eq!(
            session.output()[0],
            "error: invalid sku 'nope': missing group prefix"
        );
        assert_eq!(
            session.output()[1],
            "report products=0 customers=0 orders=0"
        );
        assert_eq!(session.output()[2], "error: unknown command 'frobnicate'");
    }
}
