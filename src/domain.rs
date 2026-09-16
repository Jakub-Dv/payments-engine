#[derive(
    serde::Deserialize, serde::Serialize, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy,
)]
#[serde(transparent)]
pub(crate) struct ClientId(pub(crate) u16);

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(serde::Deserialize, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
#[serde(transparent)]
pub(crate) struct TxId(pub(crate) u32);

impl std::fmt::Display for TxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Row {
    Deposit {
        client_id: ClientId,
        tx_id: TxId,
        amount: rust_decimal::Decimal,
    },
    Withdrawal {
        client_id: ClientId,
        tx_id: TxId,
        amount: rust_decimal::Decimal,
    },
    Dispute {
        client_id: ClientId,
        tx_id: TxId,
    },
    Resolve {
        client_id: ClientId,
        tx_id: TxId,
    },
    Chargeback {
        client_id: ClientId,
        tx_id: TxId,
    },
}

impl Row {
    pub(crate) fn ids(&self) -> (ClientId, TxId) {
        match *self {
            Self::Deposit {
                client_id, tx_id, ..
            }
            | Self::Withdrawal {
                client_id, tx_id, ..
            }
            | Self::Dispute { client_id, tx_id }
            | Self::Resolve { client_id, tx_id }
            | Self::Chargeback { client_id, tx_id } => (client_id, tx_id),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Row {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Deposit,
            Withdrawal,
            Dispute,
            Resolve,
            Chargeback,
        }

        #[derive(serde::Deserialize)]
        struct Fields {
            #[serde(rename = "type")]
            kind: Kind,
            client: ClientId,
            tx: TxId,
            amount: Option<String>,
        }

        let Fields {
            kind,
            client: client_id,
            tx: tx_id,
            amount,
        } = Fields::deserialize(deserializer)?;

        let required_amount = || {
            let text = amount
                .as_deref()
                .ok_or_else(|| <D::Error as serde::de::Error>::missing_field("amount"))?;
            // CSV's deserialize_any infers f64; parse text to preserve decimal precision.
            rust_decimal::Decimal::from_str_exact(text)
                .map_err(<D::Error as serde::de::Error>::custom)
        };

        Ok(match kind {
            Kind::Deposit => Self::Deposit {
                client_id,
                tx_id,
                amount: required_amount()?,
            },
            Kind::Withdrawal => Self::Withdrawal {
                client_id,
                tx_id,
                amount: required_amount()?,
            },
            Kind::Dispute => Self::Dispute { client_id, tx_id },
            Kind::Resolve => Self::Resolve { client_id, tx_id },
            Kind::Chargeback => Self::Chargeback { client_id, tx_id },
        })
    }
}
