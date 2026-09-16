use std::collections::BTreeMap;

use eyre::Context;

use crate::domain::{ClientId, Row, TxId};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ClientTxType {
    Deposit,
    Withdrawal,
}

#[derive(Debug, PartialEq, Eq)]
enum ClientTxState {
    Accounted,
    Disputed,
    Resolved,
    Chargedback,
}

#[derive(Default)]
struct ClientBalances {
    available: rust_decimal::Decimal,
    held: rust_decimal::Decimal,
    total: rust_decimal::Decimal,
    locked: bool,
}

struct ClientTx {
    amount: rust_decimal::Decimal,
    state: ClientTxState,
}

impl ClientTx {
    fn new_accounted(
        amount: rust_decimal::Decimal,
        tx_type: ClientTxType,
        balances: &mut ClientBalances,
    ) -> eyre::Result<ClientTx> {
        match tx_type {
            ClientTxType::Deposit => {
                balances.available += amount;
                balances.total += amount;
            }
            ClientTxType::Withdrawal => {
                let new_available = balances.available - amount;
                let new_total = balances.total - amount;
                eyre::ensure!(
                    new_available >= rust_decimal::Decimal::ZERO
                        && new_total >= rust_decimal::Decimal::ZERO,
                    "Failed to withdraw amount."
                );
                balances.available = new_available;
                balances.total = new_total;
            }
        }

        Ok(Self {
            amount: amount,
            state: ClientTxState::Accounted,
        })
    }

    fn dispute(&mut self, balances: &mut ClientBalances) {
        if self.state != ClientTxState::Accounted {
            return;
        }

        balances.available -= self.amount;
        balances.held += self.amount;

        self.state = ClientTxState::Disputed;
    }

    fn resolve(&mut self, balances: &mut ClientBalances) {
        if self.state != ClientTxState::Disputed {
            return;
        }

        balances.available += self.amount;
        balances.held -= self.amount;

        self.state = ClientTxState::Resolved;
    }

    fn chargeback(&mut self, balances: &mut ClientBalances) {
        if self.state != ClientTxState::Disputed {
            return;
        }

        balances.held -= self.amount;
        balances.total -= self.amount;

        balances.locked = true;

        self.state = ClientTxState::Chargedback;
    }
}

#[derive(Default)]
struct ClientAccount {
    txs: BTreeMap<TxId, ClientTx>,
    balances: ClientBalances,
}

impl ClientAccount {
    fn process_row(&mut self, row: Row) -> eyre::Result<()> {
        eyre::ensure!(!self.balances.locked, "Account is locked.");

        match row {
            Row::Deposit {
                client_id,
                tx_id,
                amount,
            } => self
                .deposit(tx_id, amount)
                .with_context(|| format!("Client {client_id}"))?,
            Row::Withdrawal {
                client_id,
                tx_id,
                amount,
            } => self
                .withdraw(tx_id, amount)
                .with_context(|| format!("Client {client_id}"))?,
            Row::Dispute { tx_id, .. } => self.dispute(tx_id),
            Row::Resolve { tx_id, .. } => self.resolve(tx_id),
            Row::Chargeback { tx_id, .. } => self.chargeback(tx_id),
        }

        Ok(())
    }

    fn deposit(&mut self, tx_id: TxId, amount: rust_decimal::Decimal) -> eyre::Result<()> {
        let tx = ClientTx::new_accounted(amount, ClientTxType::Deposit, &mut self.balances)?;
        self.txs.insert(tx_id, tx);
        Ok(())
    }

    fn withdraw(&mut self, tx_id: TxId, amount: rust_decimal::Decimal) -> eyre::Result<()> {
        let tx = ClientTx::new_accounted(amount, ClientTxType::Withdrawal, &mut self.balances)?;
        self.txs.insert(tx_id, tx);
        Ok(())
    }

    fn dispute(&mut self, tx_id: TxId) {
        if let Some(tx) = self.txs.get_mut(&tx_id) {
            tx.dispute(&mut self.balances);
        }
    }

    fn resolve(&mut self, tx_id: TxId) {
        if let Some(tx) = self.txs.get_mut(&tx_id) {
            tx.resolve(&mut self.balances);
        }
    }

    fn chargeback(&mut self, tx_id: TxId) {
        if let Some(tx) = self.txs.get_mut(&tx_id) {
            tx.chargeback(&mut self.balances);
        }
    }
}

#[derive(serde::Serialize, Debug)]
pub(crate) struct ClientBalancesOutput {
    #[serde(rename = "client")]
    client_id: ClientId,
    available: rust_decimal::Decimal,
    held: rust_decimal::Decimal,
    total: rust_decimal::Decimal,
    locked: bool,
}

impl From<(ClientId, ClientBalances)> for ClientBalancesOutput {
    fn from((client_id, balances): (ClientId, ClientBalances)) -> ClientBalancesOutput {
        ClientBalancesOutput {
            client_id,
            available: balances.available,
            held: balances.held,
            total: balances.total,
            locked: balances.locked,
        }
    }
}

pub(crate) struct TransactionsProcessor {
    client_accounts: BTreeMap<ClientId, ClientAccount>,
}

impl TransactionsProcessor {
    pub(crate) fn new() -> Self {
        Self {
            client_accounts: BTreeMap::new(),
        }
    }

    pub(crate) fn process_row(&mut self, row: Row) -> eyre::Result<()> {
        let account = match &row {
            Row::Deposit { client_id, .. } | Row::Withdrawal { client_id, .. } => {
                self.client_accounts.entry(*client_id).or_default()
            }
            Row::Dispute { client_id, .. }
            | Row::Resolve { client_id, .. }
            | Row::Chargeback { client_id, .. } => {
                let Some(account) = self.client_accounts.get_mut(client_id) else {
                    return Ok(());
                };
                account
            }
        };

        account.process_row(row)
    }

    pub(crate) fn get_output(self) -> Vec<ClientBalancesOutput> {
        self.client_accounts
            .into_iter()
            .map(|ca| (ca.0, ca.1.balances).into())
            .collect()
    }
}
