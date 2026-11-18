//! The line based snapshot format.
//!
//! One record per line, pipe separated, with dependent records written first
//! (`product`, then `customer`, then `order`, then `meta`).

use domain::{
    Address, Cents, Country, Currency, Customer, CustomerId, Discount, Money, Order, OrderId,
    OrderLine, OrderStatus, Product, Sku,
};

use crate::error::StorageError;
use crate::store::Store;

/// Header written at the top of every snapshot.
pub const HEADER: &str = "# shopfront store v1";

/// Renders the whole store. Records are ordered so a snapshot is stable.
pub fn encode(store: &Store) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');

    for product in store.products() {
        let price = product.unit_price;
        out.push_str(&format!(
            "product|{}|{}|{}|{}|{}|{}|{}\n",
            product.sku,
            product.name,
            price.cents,
            price.currency.code(),
            product.weight_grams,
            product.hazardous,
            store.stock_of(&product.sku)
        ));
    }

    let mut customers: Vec<&Customer> = store.customers.values().collect();
    customers.sort_by_key(|customer| customer.id);
    for customer in customers {
        let Address {
            line1,
            city,
            postal_code,
            country,
        } = &customer.billing_address;
        out.push_str(&format!(
            "customer|{}|{}|{}|{}|{}|{}|{}|{}\n",
            customer.id,
            customer.name,
            customer.email,
            line1,
            city,
            postal_code,
            country.code(),
            customer.loyalty_points
        ));
    }

    for order in store.orders() {
        let lines: Vec<String> = order.lines.iter().map(encode_line).collect();
        let Address {
            line1,
            city,
            postal_code,
            country,
        } = &order.ship_to;
        out.push_str(&format!(
            "order|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
            order.id,
            order.customer.id,
            order.currency.code(),
            order.status.spec(),
            order.discount.spec(),
            line1,
            city,
            postal_code,
            country.code(),
            lines.join(";")
        ));
    }

    out.push_str(&format!("meta|{}\n", store.next_order_id));
    out
}

/// Parses a snapshot produced by [`encode`].
pub fn decode(source: &str) -> Result<Store, StorageError> {
    let mut store = Store::new();
    for (index, raw) in source.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = trimmed.split('|').collect();
        match fields[0] {
            "product" => decode_product(&mut store, &fields, line_no)?,
            "customer" => decode_customer(&mut store, &fields, line_no)?,
            "order" => decode_order(&mut store, &fields, line_no)?,
            "meta" => {
                store.next_order_id = number(&fields, 1, line_no, "next order id")?;
            }
            other => {
                return Err(StorageError::Parse {
                    line: line_no,
                    message: format!("unknown record '{other}'"),
                });
            }
        }
    }
    Ok(store)
}

fn encode_line(line: &OrderLine) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        line.sku,
        line.quantity,
        line.unit_price.cents,
        line.weight_grams,
        line.hazardous,
        line.discount.spec()
    )
}

fn decode_line(text: &str, line_no: usize, currency: Currency) -> Result<OrderLine, StorageError> {
    let fields: Vec<&str> = text.split(':').collect();
    if fields.len() < 6 {
        return Err(StorageError::Parse {
            line: line_no,
            message: format!("malformed order line '{text}'"),
        });
    }
    let sku = Sku::parse(fields[0]).map_err(|error| parse_error(line_no, &error))?;
    let quantity: u32 = fields[1].parse().map_err(|_| StorageError::Parse {
        line: line_no,
        message: format!("quantity '{}' is not a number", fields[1]),
    })?;
    let unit_cents: Cents = fields[2].parse().map_err(|_| StorageError::Parse {
        line: line_no,
        message: format!("price '{}' is not a number", fields[2]),
    })?;
    let weight_grams: u32 = fields[3].parse().map_err(|_| StorageError::Parse {
        line: line_no,
        message: format!("weight '{}' is not a number", fields[3]),
    })?;
    let hazardous = match fields[4] {
        "true" => true,
        "false" => false,
        other => {
            return Err(StorageError::Parse {
                line: line_no,
                message: format!("flag '{other}' is not a boolean"),
            });
        }
    };
    let spec = fields[5..].join(":");
    let discount =
        Discount::parse(&spec, currency).map_err(|error| parse_error(line_no, &error))?;
    let mut line = OrderLine::new(
        sku,
        quantity,
        Money::new(unit_cents, currency),
        weight_grams,
    );
    line.hazardous = hazardous;
    line.discount = discount;
    Ok(line)
}

fn decode_product(store: &mut Store, fields: &[&str], line_no: usize) -> Result<(), StorageError> {
    let sku = Sku::parse(field(fields, 1, line_no, "sku")?)?;
    let name = field(fields, 2, line_no, "name")?.to_string();
    let cents: Cents = number(fields, 3, line_no, "price")?;
    let currency = parse_currency(field(fields, 4, line_no, "currency")?, line_no)?;
    let weight_grams: u32 = number(fields, 5, line_no, "weight")?;
    let hazardous = boolean(fields, 6, line_no, "hazardous")?;
    let stock: u32 = number(fields, 7, line_no, "stock")?;
    if store.products.contains_key(&sku) {
        return Err(StorageError::Parse {
            line: line_no,
            message: format!("duplicate product '{sku}'"),
        });
    }
    let product = Product {
        sku: sku.clone(),
        name,
        unit_price: Money::new(cents, currency),
        weight_grams,
        hazardous,
    };
    store.inventory.insert(sku.clone(), stock);
    store.products.insert(sku, product);
    Ok(())
}

fn decode_customer(store: &mut Store, fields: &[&str], line_no: usize) -> Result<(), StorageError> {
    let id = CustomerId(number(fields, 1, line_no, "customer id")?);
    let address = Address {
        line1: field(fields, 4, line_no, "address line")?.to_string(),
        city: field(fields, 5, line_no, "city")?.to_string(),
        postal_code: field(fields, 6, line_no, "postal code")?.to_string(),
        country: parse_country(field(fields, 7, line_no, "country")?, line_no)?,
    };
    let mut customer = Customer::new(
        id,
        field(fields, 2, line_no, "name")?,
        field(fields, 3, line_no, "email")?,
        address,
    );
    customer.loyalty_points = number(fields, 8, line_no, "loyalty points")?;
    if store.customers.contains_key(&id) {
        return Err(StorageError::Parse {
            line: line_no,
            message: format!("duplicate customer {id}"),
        });
    }
    store.customers.insert(id, customer);
    Ok(())
}

fn decode_order(store: &mut Store, fields: &[&str], line_no: usize) -> Result<(), StorageError> {
    let id = OrderId(number(fields, 1, line_no, "order id")?);
    let customer_id = CustomerId(number(fields, 2, line_no, "customer id")?);
    let currency = parse_currency(field(fields, 3, line_no, "currency")?, line_no)?;
    let status = OrderStatus::parse(field(fields, 4, line_no, "status")?, currency)
        .map_err(|error| parse_error(line_no, &error))?;
    let discount = Discount::parse(field(fields, 5, line_no, "discount")?, currency)
        .map_err(|error| parse_error(line_no, &error))?;
    let address = Address {
        line1: field(fields, 6, line_no, "address line")?.to_string(),
        city: field(fields, 7, line_no, "city")?.to_string(),
        postal_code: field(fields, 8, line_no, "postal code")?.to_string(),
        country: parse_country(field(fields, 9, line_no, "country")?, line_no)?,
    };
    let customer = store
        .customers
        .get(&customer_id)
        .cloned()
        .ok_or(StorageError::UnknownCustomer(customer_id.0))?;

    let mut order = Order::new(id, customer, address, currency);
    order.discount = discount;
    order.status = status;
    for chunk in field(fields, 10, line_no, "lines")?
        .split(';')
        .filter(|chunk| !chunk.is_empty())
    {
        order.add_line(decode_line(chunk, line_no, currency)?)?;
    }
    store.orders.insert(id, order);
    Ok(())
}

/// Reads one pipe separated field.
pub(crate) fn field<'a>(
    fields: &[&'a str],
    index: usize,
    line: usize,
    name: &str,
) -> Result<&'a str, StorageError> {
    fields
        .get(index)
        .copied()
        .ok_or_else(|| StorageError::Parse {
            line,
            message: format!("missing {name}"),
        })
}

fn number<T>(fields: &[&str], index: usize, line: usize, name: &str) -> Result<T, StorageError>
where
    T: std::str::FromStr,
{
    let raw = field(fields, index, line, name)?;
    raw.parse().map_err(|_| StorageError::Parse {
        line,
        message: format!("{name} '{raw}' is not a number"),
    })
}

fn boolean(fields: &[&str], index: usize, line: usize, name: &str) -> Result<bool, StorageError> {
    match field(fields, index, line, name)? {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(StorageError::Parse {
            line,
            message: format!("{name} '{other}' is not a boolean"),
        }),
    }
}

fn parse_currency(raw: &str, line: usize) -> Result<Currency, StorageError> {
    Currency::from_code(raw).map_err(|error| parse_error(line, &error))
}

fn parse_country(raw: &str, line: usize) -> Result<Country, StorageError> {
    Country::from_code(raw).map_err(|error| parse_error(line, &error))
}

fn parse_error(line: usize, error: &dyn std::fmt::Display) -> StorageError {
    StorageError::Parse {
        line,
        message: error.to_string(),
    }
}

/// Rejects text that would break the record separators.
pub(crate) fn check_text(field: &'static str, value: &str) -> Result<(), StorageError> {
    if value.contains(['|', ';']) {
        return Err(StorageError::InvalidText(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Region;

    fn populated() -> Store {
        let mut store = Store::new();
        store
            .add_product(
                Product::new(
                    Sku::parse("MUG-1").unwrap(),
                    "Blue Mug",
                    Money::new(1299, Currency::Eur),
                    400,
                ),
                25,
            )
            .unwrap();
        store
            .add_customer(Customer::new(
                CustomerId(1),
                "Ada Lovelace",
                "ada@example.com",
                Address::new("Hauptstr 1", "Berlin", "10115", Country::De),
            ))
            .unwrap();
        store
            .place_order(
                CustomerId(1),
                Discount::Percent { basis_points: 1000 },
                &[(Sku::parse("MUG-1").unwrap(), 2, Discount::None)],
            )
            .unwrap();
        store
    }

    #[test]
    fn snapshots_round_trip() {
        let store = populated();
        let text = encode(&store);
        let restored = decode(&text).unwrap();
        assert_eq!(encode(&restored), text);
        assert_eq!(restored.product_count(), 1);
        assert_eq!(restored.customer_count(), 1);
        assert_eq!(restored.order_count(), 1);
        assert_eq!(restored.total_stock(), 23);
        let order = restored.order(OrderId(1000)).unwrap();
        assert_eq!(order.lines.len(), 1);
        assert_eq!(order.lines[0].discount, Discount::None);
        assert_eq!(order.status, OrderStatus::Placed);
        assert_eq!(order.customer.region(), Region::Europe);
        assert_eq!(restored.next_order_id, 1001);
    }

    #[test]
    fn bad_lines_name_their_line_number() {
        let text = "product|MUG-1|Blue Mug|1299|EUR|400|false|25\norder|oops";
        let error = decode(text).unwrap_err();
        assert_eq!(
            error.message(),
            "snapshot line 2: order id 'oops' is not a number"
        );
        assert!(decode("nonsense|1").is_err());
        assert!(decode("product|BAD|name|1|EUR|1|maybe|1").is_err());
    }
}
