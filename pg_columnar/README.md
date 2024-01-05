# pg_columnar

This extension enables Clickhouse-level OLAP performance inside Postgres. By embedding Apache Datafusion inside Postgres,
this extension brings column-oriented storage, vectorized query execution, and SIMD instructions to Postgres tables.

# Development 

## Prerequisites

To develop the extension, first install Rust v1.73.0 using `rustup`. We will soon make the extension compatible with newer versions of Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup install 1.73.0

# We recommend setting the default version for consistency
rustup default 1.73.0
```

Note: While it is possible to install Rust via your package manager, we recommend using `rustup` as we've observed inconcistencies with Homebrew's Rust installation on macOS.

Then, install the PostgreSQL version of your choice using your system package manager. Here we provide the commands for the default PostgreSQL version used by this project:

```bash
# macOS
brew install postgresql@15

# Ubuntu
wget --quiet -O - https://www.postgresql.org/media/keys/ACCC4CF8.asc | sudo apt-key add -
sudo sh -c 'echo "deb http://apt.postgresql.org/pub/repos/apt/ $(lsb_release -cs)-pgdg main" > /etc/apt/sources.list.d/pgdg.list'
sudo apt-get update && sudo apt-get install -y postgresql-$15 postgresql-server-dev-15
```

If you are using Postgres.app to manage your macOS PostgreSQL, you'll need to add the `pg_config` binary to your path before continuing:

```bash
export PATH="$PATH:/Applications/Postgres.app/Contents/Versions/latest/bin"
```

Then, install and initialize pgrx:

```bash
# Note: Replace --pg15 with your version of Postgres, if different (i.e. --pg16, --pg14, etc.)
cargo install --locked cargo-pgrx --version 0.11.1
cargo pgrx init --pg15=`which pg_config`
```

The extension can be developed with or without SIMD enabled. Enabling SIMD improves query times by 10-20x but also significantly increases build times. For fast development iteration, we recommend disabling SIMD.

## SIMD Disabled 

To launch the extension with SIMD disabled, run

```bash
cargo pgrx run
```

## SIMD Enabled

First, switch to Rust Nightly 1.72 via:

```bash
rustup override set nightly-2023-06-01
```

Then, reinstall pgrx for the new version of Rust:

```bash
cargo install --locked cargo-pgrx --version 0.11.1 --force
```

Finally, run 

```bash
cargo pgrx run --release
```

Note that this may take several minutes to execute.

## Benchmarks

To run benchmarks locally for development, first enter the `pg_columnar/` directory before running `cargo clickbench`. This runs a minified version of the ClickBench benchmark suite on a purely in-memory version of `pg_columnar`. As of writing, this is the only functional benchmark suite as we haven't built persistence in our TableAM. Once we do, you can run the full suite using on-disk storage via `cargo clickbench_cold`.