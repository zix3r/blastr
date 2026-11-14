# Blastr

Blastr is a Rust tool that shows what a code change in a Rust project can affect: it compares two
Git revisions of a Cargo project, works out what changed structurally, and shows which other code
and tests that change can reach. That reachable set is the change's "blast radius", which is where
the name comes from.

Blastr is one program with two entry points: a desktop GUI, opened when `blastr` is started with no
command, and a CLI, used as `blastr <command>`.

## Status

Blastr is in early development. Nothing has been released yet.

## Building

Building needs the Rust toolchain. The pinned version is listed in `rust-toolchain.toml` and is
installed automatically by `rustup` the first time Cargo runs in the repository.

On Linux, the GUI needs the system packages GPUI builds against:

```sh
sudo apt-get install -y build-essential cmake clang lld pkg-config libssl-dev libzstd-dev libsqlite3-dev libgit2-dev libglib2.0-dev libfontconfig-dev libwayland-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libvulkan1 libvulkan-dev libasound2-dev
```

Build and test the workspace:

```sh
cargo build --workspace
cargo test --workspace
```

To build the binary without the GUI:

```sh
cargo build -p blastr --no-default-features
```

## Running

```sh
cargo run -p blastr
```

This opens the GUI. For the command-line interface:

```sh
cargo run -p blastr -- --help
```

This lists the available commands.

## Licence

Licensed under either of the MIT licence (`LICENSE-MIT`) or the Apache Licence 2.0
(`LICENSE-APACHE`), at your option.
