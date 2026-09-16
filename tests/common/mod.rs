use rust_decimal::Decimal;
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Account {
    client: u16,
    #[serde(deserialize_with = "decimal")]
    available: Decimal,
    #[serde(deserialize_with = "decimal")]
    held: Decimal,
    #[serde(deserialize_with = "decimal")]
    total: Decimal,
    locked: bool,
}

fn decimal<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    let text = String::deserialize(deserializer)?;
    Decimal::from_str_exact(&text).map_err(serde::de::Error::custom)
}

pub fn assert_accounts(actual: &[u8], expected: &str) -> eyre::Result<()> {
    fn parse(bytes: &[u8]) -> eyre::Result<Vec<Account>> {
        let mut reader = csv::Reader::from_reader(bytes);
        assert_eq!(
            reader.headers()?.iter().collect::<Vec<_>>(),
            ["client", "available", "held", "total", "locked"]
        );
        let mut accounts = reader
            .deserialize::<Account>()
            .collect::<Result<Vec<_>, _>>()?;
        accounts.sort_by_key(|account| account.client);
        for account in &accounts {
            assert_eq!(account.total, account.available + account.held);
        }
        Ok(accounts)
    }
    assert_eq!(parse(actual)?, parse(expected.as_bytes())?);
    Ok(())
}
