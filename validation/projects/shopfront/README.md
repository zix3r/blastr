# shopfront

A pretend online-shop back end: a catalogue of articles, customers with addresses, baskets that
become orders, the money rules around those orders (line totals, reductions, tax by region,
shipping) and a text front end that drives the whole thing from a script.

Nothing here talks to the network or the filesystem. The "database" is a set of `HashMap`s, and a
snapshot format moves it between processes.

```
crates/domain    core types: Money/Cents, Sku, Product, Customer, Address, Order, OrderLine,
                 OrderStatus, Discount, error enums. No I/O, no formatting policy.
crates/billing   pricing: line totals, reductions, the tax table, shipping quotes, invoices.
crates/storage   the in-memory store: catalogue, stock, reservations, orders, queries, snapshots.
crates/cli       the `shopfront` binary plus its command language (`add-product`, `add-customer`,
                 `place-order`, `advance`, `invoice`, `report`). `main` is four lines.
```

Dependency edges: `cli → {billing, storage}`, `storage → {billing, domain}`, `billing → domain`.

## Running it

```sh
cargo test --workspace
cargo run -p cli --bin shopfront -- \
  'add-product sku=MUG-1 name="Blue Mug" price=1299 currency=EUR weight=400 stock=25' \
  'add-customer id=1 name="Ada" email=ada@example.com line1="Hauptstr 1" city=Berlin postal=10115 country=DE' \
  'place-order customer=1 items=MUG-1:2 discount=percent:10' \
  'invoice order=1000'
```

Script syntax: one command per line, `key=value` arguments, double quotes around values with
spaces, `#` starts a comment. A failing line prints `error: …` and the script continues.

## Blastr rule coverage

| Blastr rule | Where it is exercised |
| --- | --- |
| function body | every module; check `billing/src/tax.rs:tax_for`, `storage/src/store.rs:place_order`, `cli/src/commands.rs:transition_allowed` |
| function signature (parameter added / return retyped) | `billing/src/pricing.rs:order_totals` (5 call sites: `cli/src/commands.rs` twice, `storage/src/store.rs` twice, tests), `billing/src/shipping.rs:shipping_cost`, `cli/src/commands.rs:build_status`, `cli/src/commands.rs:parse_items` |
| struct field added / removed / retyped | `domain/src/customer.rs:Address` (literals in `cli/src/commands.rs:add_customer`, `storage/src/text.rs:decode_customer`, `decode_order`; exhaustive pattern in `storage/src/text.rs:encode`; `..` pattern in `billing/src/invoice.rs:generate_invoice`) · `domain/src/order.rs:OrderLine` (literal in `storage/src/store.rs:place_order`; rest pattern in `billing/src/pricing.rs:line_total`) · `domain/src/product.rs:Product` (literal in `storage/src/text.rs:decode_product`) · `domain/src/money.rs:Money` (`cents` read/written in all four crates) · `billing/src/pricing.rs:Totals` (read by `billing/src/invoice.rs` and `cli/src/format.rs`) · `storage/src/store.rs:RegionRevenue` |
| enum variant added / removed | `domain/src/order.rs:OrderStatus` — exhaustive matches in `OrderStatus::label/is_open/spec`, `cli/src/format.rs:status_detail`, `cli/src/commands.rs:transition_allowed`; wildcard arms in `OrderStatus::tag`, `billing/src/invoice.rs:status_note`, `storage/src/store.rs:transition`; variants built in `cli/src/commands.rs:build_status` · `domain/src/discount.rs:Discount` (exhaustive in `billing/src/discount.rs:describe`, wildcard in `Discount::cut`) · `domain/src/customer.rs:Country` (`Other(String)`) · `Region` · `Carrier` · `CancelReason` · `domain/src/error.rs:DomainError` · `storage/src/error.rs:StorageError` |
| enum variant constructed in another crate | `domain/Carrier`, `CancelReason`, `OrderStatus::{Paid,Shipped,Delivered,Cancelled,Refunded}`, `Discount::{Percent,Bundle}` all built in `cli/src/commands.rs` and in `billing`/`storage` tests |
| `match` with `_` wildcard | `domain/src/order.rs:OrderStatus::tag`, `domain/src/discount.rs:Discount::cut`, `billing/src/invoice.rs:status_note`, `storage/src/store.rs:transition`, `cli/src/commands.rs:transition_allowed` |
| trait item added | not exercised: this workspace has no traits (the domain is data plus free functions) |
| `macro_rules!` body | not exercised: no macro definitions in this workspace |
| const value / const type | `billing/src/lib.rs:{BASE_SHIPPING_CENTS, PER_KG_SHIPPING_CENTS, FREE_SHIPPING_THRESHOLD_CENTS, HAZARD_SURCHARGE_CENTS, INTERNATIONAL_SURCHARGE_CENTS}` used by `billing/src/shipping.rs:shipping_cost` · `billing/src/lib.rs:MAX_LINE_QUANTITY` used by `cli/src/commands.rs:parse_items` · `billing/src/tax.rs:{TAX_TABLE, TAX_FREE_BELOW_CENTS}` used by `bracket_for`, `label_for`, `tax_for` · `domain/src/money.rs:{CENTS_PER_UNIT, BASIS_POINTS_PER_UNIT}` used by `Customer::add_loyalty`, `portion` · `domain/src/product.rs:Sku::{MAX_LEN, SEPARATOR}` · `storage/src/store.rs:FIRST_ORDER_ID` · `cli/src/commands.rs:LOW_STOCK_THRESHOLD` · `cli/src/lib.rs:HELP` |
| type alias | `domain/src/money.rs:Cents = i64`, used in ~60 signatures across all four crates |
| visibility narrowed (`pub` → `pub(crate)`) | `domain/src/money.rs:round_div` (user `portion`), `billing/src/discount.rs:percent_text` (user `describe`), `billing/src/shipping.rs:kilograms` (user `shipping_cost`), `storage/src/store.rs:Store` fields (users `storage/src/text.rs:encode/decode`) and `validate_items` (user `place_order`), `storage/src/text.rs:{field, check_text}` (users `decode`, `Store::add_product`), `cli/src/command.rs:tokenize` (user `Command::parse`) |
| symbol removed / renamed | `domain::money::{portion,sum_by}`, `billing::pricing::{line_total,line_gross}`, `storage::Store::{reserve,commit,release}`, `cli::format::money` each have 2–5 call sites |
| attributes / derives | `#[derive]` on every domain type; `OrderStatus: Clone` is what `cli/src/commands.rs:advance` and `storage/src/store.rs:transition` rely on |
| deep call chains (4–6+, here up to 10) | `cli::run → Session::run → Session::execute → Session::report → Store::revenue_by_region → billing::order_totals → billing::tax::tax_for → billing::tax::tax_rate_bp → domain::portion → domain::money::round_div`; similarly `… → Session::invoice → generate_invoice → pricing::line_total → discount::apply → Discount::cut → Money::percent_bp → portion` |
| tests that fail transitively | `cli/tests/e2e.rs` asserts whole script output; `billing/tests/invoice.rs` asserts a full invoice string, so a change in `portion`, `tax_for` or `kilograms` fails it |

## Tests

| Crate | Unit tests | Integration tests |
| --- | --- | --- |
| `domain` | `money`, `product`, `customer`, `discount`, `order` inline modules | — |
| `billing` | `discount`, `tax`, `shipping`, `pricing`, `invoice` inline modules | `tests/pricing.rs`, `tests/invoice.rs` |
| `storage` | `store`, `text` inline modules | `tests/store.rs`, `tests/persistence.rs` |
| `cli` | `command`, `commands`, `format`, crate root | `tests/e2e.rs` (whole scripts, exact output) |

No doctests anywhere, so the workspace also covers the "no doctest analysis" case.
