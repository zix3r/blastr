//! The `shopfront` front end: a session, a command language and its output.
//!
//! The binary in `src/main.rs` is a thin wrapper around [`run`]; everything a
//! test can reach lives here.

pub mod command;
pub mod commands;
pub mod format;

pub use command::{Command, CommandError};
pub use commands::{LOW_STOCK_THRESHOLD, Session};

/// What the binary prints when started without arguments.
pub const HELP: &str = "\
shopfront - a tiny shop back end

commands:
  add-product sku=MUG-1 name=\"Blue Mug\" price=1299 currency=EUR weight=400 stock=25 [hazard=on]
  add-customer id=1 name=\"Ada\" email=ada@example.com line1=\"Hauptstr 1\" city=Berlin postal=10115 country=DE
  place-order customer=1 items=MUG-1:2,LAMP-2:1 [discount=percent:10]
  advance order=1000 to=paid ref=PAY-1 [amount=1299]
  advance order=1000 to=shipped carrier=courier:ups:express tracking=1Z9
  advance order=1000 to=delivered [day=3]
  advance order=1000 to=cancelled reason=out-of-stock:MUG-1
  invoice order=1000
  report [threshold=5]

Every command takes key=value pairs; quote values that contain spaces.";

/// Runs a script through a fresh session and returns the rendered output.
pub fn run_script(script: &str) -> String {
    let mut session = Session::new();
    session.run(script);
    session.into_output()
}

/// Runs the command line arguments as one script line each.
///
/// Returns the process exit code: 0 when everything succeeded, 1 otherwise.
pub fn run(args: Vec<String>) -> u8 {
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        println!("{HELP}");
        return 0;
    }
    let script = args.join("\n");
    let mut session = Session::new();
    session.run(&script);
    let failures = session.errors();
    print!("{}", session.into_output());
    if failures > 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scripts_produce_no_output() {
        assert_eq!(run_script(""), "");
        assert_eq!(run_script("# only a comment\n\n"), "");
    }

    #[test]
    fn failing_scripts_still_render_their_errors() {
        let output = run_script("report\ninvoice order=999");
        assert_eq!(
            output,
            "report products=0 customers=0 orders=0\nerror: unknown order #999\n"
        );
    }
}
