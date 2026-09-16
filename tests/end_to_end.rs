use std::{
    io::Write,
    process::{Command, Output},
};

use proptest::{prelude::*, test_runner::TestCaseError};
use tempfile::{NamedTempFile, tempdir};

mod common;

const BINARY: &str = env!("CARGO_BIN_EXE_payments-engine");
const INPUT_HEADER: &str = "type,client,tx,amount\n";
const OUTPUT_HEADER: &str = "client,available,held,total,locked\n";

proptest! {
    #[test]
    fn prop_csv_funding_histories_match_integer_balances(
        steps in prop::collection::vec(
            (
                prop::sample::select(vec![0_u16, 1, u16::MAX]),
                any::<bool>(),
                prop_oneof![3 => Just(0_i64), 7 => -1_000_000_000_000_i64..=1_000_000_000_000],
                any::<bool>(),
            ),
            0..=64,
        ),
    ) {
        use std::{collections::BTreeMap, fmt::Write as _};
        use rust_decimal::Decimal;

        let mut input = String::from(INPUT_HEADER);
        let mut balances = BTreeMap::<u16, i128>::new();
        let mut rejected = 0_usize;
        for (tx, (client, withdrawal, units, negative_zero)) in (1_u32..).zip(steps) {
            let kind = if withdrawal { "withdrawal" } else { "deposit" };
            let amount = Decimal::new(units, 4);
            let sign = if units == 0 && negative_zero { "-" } else { "" };
            writeln!(input, " {kind} , {client} , {tx} , {sign}{amount} ")
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let balance = balances.entry(client).or_default();
            if units < 0 || (withdrawal && *balance < i128::from(units)) {
                rejected += 1;
            } else if withdrawal {
                *balance -= i128::from(units);
            } else {
                *balance += i128::from(units);
            }
        }

        let output = run(&input).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        prop_assert!(output.status.success(), "{}", stderr);
        prop_assert_eq!(stderr.matches("transaction rejected").count(), rejected);
        let mut expected = String::from(OUTPUT_HEADER);
        for (client, units) in balances {
            let balance = Decimal::from_i128_with_scale(units, 4);
            writeln!(expected, "{client},{balance},0,{balance},false")
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
        }
        common::assert_accounts(&output.stdout, &expected)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

fn run(input: &str) -> eyre::Result<Output> {
    let mut file = NamedTempFile::new()?;
    file.write_all(input.as_bytes())?;
    Ok(Command::new(BINARY)
        .arg(file.path())
        // Keep captured log fields independent of the caller's terminal settings.
        .env("NO_COLOR", "1")
        .output()?)
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fixture_matches_expected_accounts_and_rejections_stay_on_stderr() -> eyre::Result<()> {
    let output = run(include_str!("fixtures/transactions.csv"))?;
    assert_success(&output);
    common::assert_accounts(&output.stdout, include_str!("fixtures/accounts.csv"))?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("transaction rejected"), "{stderr}");
    assert!(stderr.contains("client=42"), "{stderr}");
    assert!(stderr.contains("tx=4"), "{stderr}");
    assert!(stderr.contains("InsufficientFunds"), "{stderr}");
    Ok(())
}

#[test]
fn dispute_resolve_and_chargeback_have_distinct_outcomes() -> eyre::Result<()> {
    for (operations, expected) in [
        ("dispute,1,1,\n", "1,0,10,10,false\n"),
        ("dispute,1,1,\nresolve,1,1,\n", "1,10,0,10,false\n"),
        ("dispute,1,1,\nchargeback,1,1,\n", "1,0,0,0,true\n"),
        (
            "dispute,1,1,\nresolve,1,1,\nchargeback,1,1,\n",
            "1,10,0,10,false\n",
        ),
        ("resolve,1,1,\nchargeback,1,1,\n", "1,10,0,10,false\n"),
    ] {
        let output = run(&format!("{INPUT_HEADER}deposit,1,1,10\n{operations}"))?;
        assert_success(&output);
        common::assert_accounts(&output.stdout, &format!("{OUTPUT_HEADER}{expected}"))?;
        assert!(output.stderr.is_empty());
    }
    Ok(())
}

#[test]
fn exact_withdrawals_and_four_decimal_arithmetic_are_preserved() -> eyre::Result<()> {
    let output = run(
        " type , client , tx , amount \n deposit , 65535 , 4294967295 , 1.2345 \n\
         withdrawal,65535,1,1.2345\ndeposit,2,2,0.0001\ndeposit,2,3,0.0002\n",
    )?;
    assert_success(&output);
    common::assert_accounts(
        &output.stdout,
        &format!("{OUTPUT_HEADER}65535,0,0,0,false\n2,0.0003,0,0.0003,false\n"),
    )?;
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn every_operation_on_a_locked_account_is_rejected() -> eyre::Result<()> {
    let prefix = format!(
        "{INPUT_HEADER}deposit,1,1,10\ndeposit,1,2,20\ndeposit,1,3,30\n\
         dispute,1,1,\ndispute,1,2,\nchargeback,1,1,\n"
    );
    for operation in [
        "deposit,1,4,5\n",
        "withdrawal,1,4,5\n",
        "dispute,1,3,\n",
        "resolve,1,2,\n",
        "chargeback,1,2,\n",
        "chargeback,1,1,\n",
    ] {
        let output = run(&format!("{prefix}{operation}"))?;
        assert_success(&output);
        common::assert_accounts(&output.stdout, &format!("{OUTPUT_HEADER}1,30,20,50,true\n"))?;
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("AccountLocked"), "{stderr}");
        assert!(stderr.contains("client=1"), "{stderr}");
    }
    Ok(())
}

#[test]
fn large_decimal_input_is_never_converted_through_floating_point() -> eyre::Result<()> {
    let output = run(&format!(
        "{INPUT_HEADER}deposit,1,1,12345678901234567890.1234\nwithdrawal,1,2,0.0001\n"
    ))?;
    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("{OUTPUT_HEADER}1,12345678901234567890.1233,0,12345678901234567890.1233,false\n")
    );
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn unknown_and_cross_client_references_are_ignored() -> eyre::Result<()> {
    let output = run(&format!(
        "{INPUT_HEADER}deposit,1,1,10\ndeposit,2,2,20\n\
         dispute,2,1,\nresolve,2,1,\nchargeback,2,1,\n\
         dispute,1,99,\nresolve,1,99,\nchargeback,1,99,\n\
         dispute,3,1,\nresolve,3,1,\nchargeback,3,1,\n"
    ))?;
    assert_success(&output);
    common::assert_accounts(
        &output.stdout,
        &format!("{OUTPUT_HEADER}1,10,0,10,false\n2,20,0,20,false\n"),
    )?;
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn invalid_amounts_are_reported_and_later_rows_are_processed() -> eyre::Result<()> {
    let output = run(&format!(
        "{INPUT_HEADER}deposit,1,1,-1\nwithdrawal,1,2,-1\n\
         deposit,1,3,0.00001\ndeposit,1,4,2\n"
    ))?;
    assert_success(&output);
    common::assert_accounts(&output.stdout, &format!("{OUTPUT_HEADER}1,2,0,2,false\n"))?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(stderr.matches("transaction rejected").count(), 3);
    assert!(stderr.contains("InvalidAmount"));
    Ok(())
}

#[test]
fn malformed_csv_fails_without_emitting_partial_accounts() -> eyre::Result<()> {
    for invalid in [
        "unknown,1,2,1",
        "deposit,1,2,",
        "deposit,1,2,nope",
        "deposit,65536,2,1",
        "deposit,1,4294967296,1",
        "dispute,1,1",
    ] {
        let output = run(&format!("{INPUT_HEADER}deposit,1,1,10\n{invalid}\n"))?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("processing input"));
        assert!(stderr.contains("deserializing transaction failed"));
        assert!(stderr.contains("line: 3"));
    }
    Ok(())
}

#[test]
fn missing_file_errors_only_go_to_stderr() -> eyre::Result<()> {
    let directory = tempdir()?;
    let missing = directory.path().join("missing.csv");
    let output = Command::new(BINARY).arg(&missing).output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("opening input"));
    assert!(stderr.contains("missing.csv"));
    Ok(())
}

#[test]
fn cli_requires_exactly_one_file_argument() -> eyre::Result<()> {
    for args in [vec![], vec!["one.csv", "two.csv"]] {
        let output = Command::new(BINARY).args(args).output()?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr)?.contains("Usage:"));
    }
    Ok(())
}

#[test]
fn empty_input_and_header_only_input_still_emit_the_output_schema() -> eyre::Result<()> {
    for input in ["", INPUT_HEADER] {
        let output = run(input)?;
        assert_success(&output);
        assert_eq!(output.stdout, OUTPUT_HEADER.as_bytes());
        assert!(output.stderr.is_empty());
    }
    Ok(())
}

#[test]
fn larger_input_accumulates_exactly() -> eyre::Result<()> {
    use std::fmt::Write as _;

    let mut input = String::from(INPUT_HEADER);
    for tx in 1..=10_000 {
        writeln!(input, "deposit,1,{tx},0.0001")?;
    }
    let output = run(&input)?;
    assert_success(&output);
    common::assert_accounts(&output.stdout, &format!("{OUTPUT_HEADER}1,1,0,1,false\n"))?;
    assert!(output.stderr.is_empty());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn output_device_failure_is_reported_without_panicking() -> eyre::Result<()> {
    let mut input = NamedTempFile::new()?;
    writeln!(input, "{INPUT_HEADER}deposit,1,1,10")?;
    let output = Command::new(BINARY)
        .arg(input.path())
        .stdout(std::fs::File::options().write(true).open("/dev/full")?)
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("flushing account output failed"));
    assert!(!stderr.contains("panicked"));
    Ok(())
}
