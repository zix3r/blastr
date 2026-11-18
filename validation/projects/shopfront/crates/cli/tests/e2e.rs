//! End-to-end runs: a whole script through the command layer, compared against
//! the exact text the shop prints.

use cli::{LOW_STOCK_THRESHOLD, run_script};

const SHOP_SCRIPT: &str = "\
# open the shop
add-product sku=MUG-1 name=\"Blue Mug\" price=1299 currency=EUR weight=400 stock=25
add-product sku=LAMP-2 name=\"Desk Lamp\" price=4000 currency=EUR weight=1200 stock=4
add-customer id=1 name=\"Ada Lovelace\" email=ada@example.com line1=\"Hauptstr 1\" city=Berlin postal=10115 country=DE
place-order customer=1 items=MUG-1:2,LAMP-2:1 discount=percent:10
invoice order=1000
report
";

const SHOP_OUTPUT: &str = "\
added product MUG-1 (Blue Mug) 12.99 EUR stock=25
added product LAMP-2 (Desk Lamp) 40.00 EUR stock=4
added customer 1 Ada Lovelace region=Europe points=0
order #1000 total=85.05 EUR
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
report products=2 customers=1 orders=1
revenue: Europe orders=1 total=85.05 EUR
low-stock: LAMP-2 stock=3
";

#[test]
fn a_shopping_script_prints_the_expected_text() {
    assert_eq!(run_script(SHOP_SCRIPT), SHOP_OUTPUT);
    assert_eq!(LOW_STOCK_THRESHOLD, 5);
}

const LIFECYCLE_SCRIPT: &str = "\
add-product sku=FUEL-1 name=\"Camp Fuel\" price=900 currency=GBP weight=1500 stock=2 hazard=on
add-product sku=MUG-1 name=\"Blue Mug\" price=1299 currency=GBP weight=400 stock=10
add-customer id=2 name=\"Bob\" email=bob@example.com line1=\"1 High St\" city=London postal=SW1A country=GB
place-order customer=2 items=FUEL-1:1,MUG-1:2:percent:5
advance order=1000 to=paid ref=PAY-77
advance order=1000 to=shipped carrier=courier:ups:express tracking=1Z9
advance order=1000 to=delivered day=2
advance order=1000 to=refunded reason=payment:51
report
";

const LIFECYCLE_OUTPUT: &str = "\
added product FUEL-1 (Camp Fuel) 9.00 GBP stock=2
added product MUG-1 (Blue Mug) 12.99 GBP stock=10
added customer 2 Bob region=Domestic points=0
order #1000 total=57.51 GBP
order #1000 status=paid (paid 57.51 GBP via PAY-77)
order #1000 status=shipped (handed to ups as 1Z9)
order #1000 status=delivered (delivered after 2 days)
order #1000 status=refunded (refunded 57.51 GBP: payment failed (code 51))
report products=2 customers=1 orders=1
revenue: Domestic orders=1 total=57.51 GBP
low-stock: FUEL-1 stock=1
";

#[test]
fn an_order_walks_its_whole_lifecycle() {
    assert_eq!(run_script(LIFECYCLE_SCRIPT), LIFECYCLE_OUTPUT);
}

#[test]
fn scripts_report_errors_and_carry_on() {
    let script = "\
add-customer id=1 name=\"Ada\" email=ada@example.com line1=\"Hauptstr 1\" city=Berlin postal=10115 country=DE
place-order customer=1 items=MUG-1:1
place-order customer=1 items=MUG-1:2
add-product sku=MUG-1 name=\"Blue Mug\" price=1299 currency=EUR weight=400 stock=3
place-order customer=1 items=MUG-1:2
place-order customer=1 items=NOPE-1:1
place-order customer=1 items=MUG-1:99
advance order=1000 to=shipped tracking=T1
invoice order=1000
report threshold=1
";
    let expected = "\
added customer 1 Ada region=Europe points=0
error: unknown product 'MUG-1'
error: unknown product 'MUG-1'
added product MUG-1 (Blue Mug) 12.99 EUR stock=3
order #1000 total=44.11 EUR
error: unknown product 'NOPE-1'
error: only 1 of 'MUG-1' available, 99 requested
error: cannot go from Placed to Shipped
INVOICE #1000
customer: Ada <ada@example.com>
ship-to: Hauptstr 1, Berlin 10115 DE
destination: 10115 Berlin DE
status: Placed
line: 2 x MUG-1 @ 12.99 EUR = 25.98 EUR
subtotal: 25.98 EUR
tax (VAT 19%): 4.94 EUR
shipping: 13.19 EUR
total: 44.11 EUR
report products=1 customers=1 orders=1
revenue: Europe orders=1 total=44.11 EUR
low-stock: MUG-1 stock=1
";
    assert_eq!(run_script(script), expected);
}
