# Payments engine

A synchronous Rust program that reads transaction CSV and writes account balances to stdout.

## Run

```sh
cargo build
cargo run --release -- transactions.csv > accounts.csv
```

The input path is the single positional argument. Diagnostics go to stderr. A complete
example and expected balances are in `tests/fixtures/transactions.csv` and
`tests/fixtures/accounts.csv`.

Input columns are `type,client,tx,amount`; column order can vary and surrounding
whitespace is trimmed. Client IDs are `u16`, transaction IDs are `u32`, and amounts
are decimal values with at most four fractional places. Disputes, resolutions, and
chargebacks have an empty amount field, including the trailing comma:

```csv
type,client,tx,amount
deposit,7,91,5.1250
dispute,7,91,
resolve,7,91,
```

Output columns are `client,available,held,total,locked`. Trailing decimal zeroes are
not significant. Empty input produces just the output header.

## Transaction behavior and assumptions

- Deposits increase available funds. Withdrawals require sufficient available
  funds; withdrawing the exact balance is allowed. Zero amounts are allowed.
- A dispute moves the original transaction amount from available to held.
  Resolution reverses that move. Chargeback removes the held amount and freezes
  the entire account.
- Both successful deposits and withdrawals can be disputed, using the same
  balance adjustments. A spent deposit can still be disputed, so available funds
  and, after chargeback, total funds can become negative.
- A transaction can follow `Accounted -> Disputed -> Resolved` or
  `Accounted -> Disputed -> ChargedBack`. Resolution ends that transaction's
  dispute lifecycle: later disputes on it are ignored.
- Unknown transaction references, references belonging to another client, and
  invalid dispute transitions are ignored. These references do not create new
  accounts. A deposit or withdrawal creates an account if necessary.
- Once an account is frozen, all subsequent operations on it are rejected.
- Input order defines transaction order. New deposit/withdrawal transaction IDs
  are assumed globally unique; duplicate funding transactions are not deduplicated.

## Errors and numerical safety

Expected rejections (locked accounts, insufficient funds, invalid amounts, or
unrepresentable balance changes) use typed errors carrying the client ID,
transaction ID, and cause. They leave account balances and transaction history
unchanged, are logged, and processing continues. For a newly encountered client,
a rejected funding operation can leave an empty account.

Negative amounts and amounts with more than four fractional places are rejected.
Arithmetic must preserve the exact decimal value: operations that overflow or
would round away precision at the decimal type's limit are rejected atomically.
CSV amounts are parsed directly from text without floating-point conversion.
Total is derived from available plus held, rather than stored separately.

Malformed CSV, missing or duplicated required columns, invalid IDs, missing
funding amounts, and input/output failures are fatal. The CLI exits nonzero and
reports the error chain to stderr. Account output begins only after all input
has been processed. A write failure can still leave partial output; exit status
must be checked. The output writer is explicitly flushed.

## Structure

- `domain.rs`: typed IDs and CSV row deserialization.
- `tx_processor.rs`: account routing, account mutations, transaction state, and
  typed rejections. Account operations share one frozen-account check.
- `lib.rs`: `process(input: impl Read, output: impl Write)`, for files or
  in-memory streams.
- `main.rs`: argument parsing, file opening, stderr logging, and exit status.

Input is processed one record at a time. Successful transaction history stays
in memory to support later references, so storage is O(transactions + clients),
rather than constant memory. Accounts and histories use `BTreeMap` for predictable
ordering and logarithmic lookups. Output is produced through an iterator without
collecting a second account list. There is no async runtime or external service.

## Verification

```sh
cargo fmt --check
cargo clippy -r --tests -- -D warnings
cargo test
```

With [cargo-nextest](https://nexte.st/docs/installation/pre-built-binaries/)
installed, run the unit and integration suites with:

```sh
cargo nextest run
cargo test --doc
```

The configuration in `.config/nextest.toml` runs all tests even after a failure,
disables retries, and starts terminating tests after 60 seconds.

Clippy's standard and pedantic groups are enabled. Unwraps, expects, debug macros,
and unfinished TODO macros are denied; unsafe code is forbidden.

The suite includes:

- Domain tests for state transitions, atomic rejection, balance boundaries,
  decimal precision, frozen accounts, and client isolation.
- Property tests for negative and zero amounts, zero-value dispute lifecycles,
  legitimate negative balances, and arithmetic near decimal limits. Generated
  funding CSVs also run through the real CLI, including signed zero amounts.
- Stream tests with injected read, write, and flush errors, including checks that
  structured causes remain available.
- End-to-end tests launching the actual binary against temporary CSV files:
  fixtures, all transaction types, malformed input, argument handling, decimal
  arithmetic, larger input, and stdout/stderr separation. A Linux-specific test
  also verifies a failing output device using `/dev/full`.

The property tests use [Proptest](https://docs.rs/proptest/latest/proptest/),
with 256 generated cases per property by default. Increase the case count and
set a seed for a reproducible run:

```sh
PROPTEST_CASES=2048 PROPTEST_RNG_SEED=20260916 cargo nextest run --release -E 'test(prop_)'
```

Failures are shrunk to smaller counterexamples and their seeds are saved for
replay. Keep any generated `proptest-regressions` files in version control.
