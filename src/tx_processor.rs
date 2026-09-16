use std::collections::BTreeMap;

use rust_decimal::Decimal;
use thiserror::Error;

use crate::domain::{ClientId, Row, TxId};

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum RejectionReason {
    #[error("account is locked")]
    AccountLocked,
    #[error("insufficient funds: available {available}, requested {requested}")]
    InsufficientFunds {
        available: Decimal,
        requested: Decimal,
    },
    #[error("amount {amount} must be non-negative with at most four decimal places")]
    InvalidAmount { amount: Decimal },
    #[error("resulting balances cannot be represented exactly within the supported range")]
    BalanceOverflow,
}

#[derive(Debug, Error)]
#[error("client {client_id}, transaction {tx_id}: {reason}")]
pub(crate) struct Rejection {
    pub(crate) client_id: ClientId,
    pub(crate) tx_id: TxId,
    #[source]
    reason: RejectionReason,
}

#[derive(Debug, PartialEq, Eq)]
enum ClientTxState {
    Accounted,
    Disputed,
    Resolved,
    ChargedBack,
}

#[derive(Default, Debug, PartialEq, Eq)]
struct ClientBalances {
    available: Decimal,
    held: Decimal,
    locked: bool,
}

impl ClientBalances {
    fn adjusted(
        &self,
        available_delta: Decimal,
        held_delta: Decimal,
    ) -> Result<Self, RejectionReason> {
        // checked_add can round at the precision limit; reversibility also checks exactness.
        fn add_exact(left: Decimal, right: Decimal) -> Result<Decimal, RejectionReason> {
            let sum = left
                .checked_add(right)
                .ok_or(RejectionReason::BalanceOverflow)?;
            if sum.checked_sub(left) != Some(right) || sum.checked_sub(right) != Some(left) {
                return Err(RejectionReason::BalanceOverflow);
            }
            Ok(sum)
        }

        let available = add_exact(self.available, available_delta)?;
        let held = add_exact(self.held, held_delta)?;
        add_exact(available, held)?;
        Ok(Self {
            available,
            held,
            locked: self.locked,
        })
    }
}

struct ClientTx {
    amount: Decimal,
    state: ClientTxState,
}

impl ClientTx {
    fn new(amount: Decimal) -> Self {
        Self {
            amount,
            state: ClientTxState::Accounted,
        }
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
        balances.locked = true;
        self.state = ClientTxState::ChargedBack;
    }
}

#[derive(Default)]
struct ClientAccount {
    txs: BTreeMap<TxId, ClientTx>,
    balances: ClientBalances,
}

impl ClientAccount {
    fn process_row(&mut self, row: Row) -> Result<(), RejectionReason> {
        if self.balances.locked {
            return Err(RejectionReason::AccountLocked);
        }

        self.validate_reference_transition(row)?;

        match row {
            Row::Deposit { tx_id, amount, .. } => self.deposit(tx_id, amount)?,
            Row::Withdrawal { tx_id, amount, .. } => self.withdraw(tx_id, amount)?,
            Row::Dispute { tx_id, .. } => self.dispute(tx_id),
            Row::Resolve { tx_id, .. } => self.resolve(tx_id),
            Row::Chargeback { tx_id, .. } => self.chargeback(tx_id),
        }

        Ok(())
    }

    fn validate_reference_transition(&self, row: Row) -> Result<(), RejectionReason> {
        let (_, tx_id) = row.ids();
        let Some(tx) = self.txs.get(&tx_id) else {
            return Ok(());
        };
        let (available_delta, held_delta) = match (row, &tx.state) {
            (Row::Dispute { .. }, ClientTxState::Accounted) => (-tx.amount, tx.amount),
            (Row::Resolve { .. }, ClientTxState::Disputed) => (tx.amount, -tx.amount),
            (Row::Chargeback { .. }, ClientTxState::Disputed) => (Decimal::ZERO, -tx.amount),
            _ => return Ok(()),
        };
        // Reject numeric failures before calling the infallible transition methods.
        self.balances.adjusted(available_delta, held_delta)?;
        Ok(())
    }

    fn validate_amount(amount: Decimal) -> Result<(), RejectionReason> {
        if amount < Decimal::ZERO || amount.scale() > 4 {
            return Err(RejectionReason::InvalidAmount { amount });
        }
        Ok(())
    }

    fn deposit(&mut self, tx_id: TxId, amount: Decimal) -> Result<(), RejectionReason> {
        Self::validate_amount(amount)?;
        self.balances = self.balances.adjusted(amount, Decimal::ZERO)?;
        self.txs.insert(tx_id, ClientTx::new(amount));
        Ok(())
    }

    fn withdraw(&mut self, tx_id: TxId, amount: Decimal) -> Result<(), RejectionReason> {
        Self::validate_amount(amount)?;
        if self.balances.available < amount {
            return Err(RejectionReason::InsufficientFunds {
                available: self.balances.available,
                requested: amount,
            });
        }

        self.balances = self.balances.adjusted(-amount, Decimal::ZERO)?;
        self.txs.insert(tx_id, ClientTx::new(amount));
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
    available: Decimal,
    held: Decimal,
    total: Decimal,
    locked: bool,
}

impl From<(ClientId, ClientBalances)> for ClientBalancesOutput {
    fn from((client_id, balances): (ClientId, ClientBalances)) -> Self {
        Self {
            client_id,
            available: balances.available,
            held: balances.held,
            total: balances.available + balances.held,
            locked: balances.locked,
        }
    }
}

#[derive(Default)]
pub(crate) struct TransactionsProcessor {
    client_accounts: BTreeMap<ClientId, ClientAccount>,
}

impl TransactionsProcessor {
    pub(crate) fn process_row(&mut self, row: Row) -> Result<(), Rejection> {
        let (client_id, tx_id) = row.ids();
        let account = match &row {
            Row::Deposit { .. } | Row::Withdrawal { .. } => {
                self.client_accounts.entry(client_id).or_default()
            }
            Row::Dispute { .. } | Row::Resolve { .. } | Row::Chargeback { .. } => {
                let Some(account) = self.client_accounts.get_mut(&client_id) else {
                    return Ok(());
                };
                account
            }
        };

        account.process_row(row).map_err(|reason| Rejection {
            client_id,
            tx_id,
            reason,
        })
    }

    pub(crate) fn into_output(self) -> impl Iterator<Item = ClientBalancesOutput> {
        self.client_accounts
            .into_iter()
            .map(|(client_id, account)| (client_id, account.balances).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod properties {
        use proptest::{prelude::*, test_runner::TestCaseError};

        use super::*;

        const MAX_MANTISSA: i128 = (1_i128 << 96) - 1;

        fn decimals() -> impl Strategy<Value = Decimal> {
            (
                prop_oneof![
                    2 => Just(0_i128),
                    2 => -10_000_i128..=10_000,
                    1 => Just(MAX_MANTISSA),
                    1 => Just(-MAX_MANTISSA),
                    4 => -MAX_MANTISSA..=MAX_MANTISSA,
                ],
                0_u32..=4,
            )
                .prop_map(|(mantissa, scale)| Decimal::from_i128_with_scale(mantissa, scale))
        }

        fn fixed_units(value: Decimal) -> i128 {
            value.mantissa() * 10_i128.pow(4 - value.scale())
        }

        fn representable(mut units: i128) -> bool {
            let mut scale = 4;
            while scale > 0 && units % 10 == 0 {
                units /= 10;
                scale -= 1;
            }
            units.abs() <= MAX_MANTISSA
        }

        proptest! {
            #[test]
            fn prop_negative_funding_is_rejected_atomically(
                initial in 0_i64..=1_000_000_000,
                magnitude in 1_i64..=i64::MAX,
                scale in 0_u32..=4,
            ) {
                let mut account = ClientAccount::default();
                prop_assert_eq!(account.process_row(deposit(1, initial)), Ok(()));
                let amount = -Decimal::new(magnitude, scale);
                for row in [
                    Row::Deposit { client_id: ClientId(1), tx_id: TxId(2), amount },
                    Row::Withdrawal { client_id: ClientId(1), tx_id: TxId(2), amount },
                ] {
                    prop_assert_eq!(account.process_row(row), Err(RejectionReason::InvalidAmount { amount }));
                    prop_assert_eq!(account.balances.available, Decimal::from(initial));
                    prop_assert_eq!(account.balances.held, Decimal::ZERO);
                    prop_assert!(!account.balances.locked);
                    prop_assert_eq!(account.txs.len(), 1);
                    prop_assert_eq!(&account.txs[&TxId(1)].state, &ClientTxState::Accounted);
                    prop_assert_eq!(account.txs[&TxId(1)].amount, Decimal::from(initial));
                }
            }

            #[test]
            fn prop_zero_transactions_preserve_balances_and_follow_the_dispute_lifecycle(
                initial in prop_oneof![Just(0_i64), 1_i64..=1_000_000_000],
                scale in 0_u32..=4,
                is_withdrawal in any::<bool>(),
                reverse in any::<bool>(),
            ) {
                let mut account = ClientAccount::default();
                prop_assert_eq!(account.process_row(deposit(1, initial)), Ok(()));
                let amount = Decimal::new(0, scale);
                let row = if is_withdrawal {
                    Row::Withdrawal { client_id: ClientId(1), tx_id: TxId(2), amount }
                } else {
                    Row::Deposit { client_id: ClientId(1), tx_id: TxId(2), amount }
                };
                prop_assert_eq!(account.process_row(row), Ok(()));
                prop_assert_eq!(account.txs.len(), 2);
                for row in [dispute(2), dispute(2), if reverse { chargeback(2) } else { resolve(2) }] {
                    prop_assert_eq!(account.process_row(row), Ok(()));
                    prop_assert_eq!(account.balances.available, Decimal::from(initial));
                    prop_assert_eq!(account.balances.held, Decimal::ZERO);
                }
                prop_assert_eq!(account.balances.locked, reverse);
                let expected_state = if reverse { ClientTxState::ChargedBack } else { ClientTxState::Resolved };
                prop_assert_eq!(&account.txs[&TxId(2)].state, &expected_state);
            }

            #[test]
            fn prop_disputing_spent_funds_can_produce_negative_balances(
                units in 1_i64..=1_000_000_000_000,
                reverse in any::<bool>(),
            ) {
                let amount = Decimal::new(units, 4);
                let mut account = ClientAccount::default();
                for row in [
                    Row::Deposit { client_id: ClientId(1), tx_id: TxId(1), amount },
                    Row::Withdrawal { client_id: ClientId(1), tx_id: TxId(2), amount },
                    dispute(1),
                ] {
                    prop_assert_eq!(account.process_row(row), Ok(()));
                }
                prop_assert_eq!(account.balances.available, -amount);
                prop_assert_eq!(account.balances.held, amount);
                let row = if reverse { chargeback(1) } else { resolve(1) };
                prop_assert_eq!(account.process_row(row), Ok(()));
                prop_assert_eq!(account.balances.available, if reverse { -amount } else { Decimal::ZERO });
                prop_assert_eq!(account.balances.held, Decimal::ZERO);
                prop_assert_eq!(account.balances.locked, reverse);
            }

            #[test]
            fn prop_balance_arithmetic_matches_exact_integer_units(
                available in decimals(),
                available_delta in decimals(),
                held_delta in decimals(),
            ) {
                let held_delta = held_delta.abs();
                let balances = ClientBalances { available, ..ClientBalances::default() };
                let expected_available = fixed_units(available) + fixed_units(available_delta);
                let expected_held = fixed_units(held_delta);
                let expected_total = expected_available + expected_held;
                let actual = balances.adjusted(available_delta, held_delta);
                if [expected_available, expected_held, expected_total].into_iter().all(representable) {
                    let result = actual.map_err(|error| TestCaseError::fail(error.to_string()))?;
                    prop_assert_eq!(fixed_units(result.available), expected_available);
                    prop_assert_eq!(fixed_units(result.held), expected_held);
                    prop_assert_eq!(fixed_units(result.available + result.held), expected_total);
                } else {
                    prop_assert_eq!(actual, Err(RejectionReason::BalanceOverflow));
                }
            }
        }
    }

    fn deposit(tx: u32, amount: i64) -> Row {
        Row::Deposit {
            client_id: ClientId(1),
            tx_id: TxId(tx),
            amount: Decimal::from(amount),
        }
    }

    fn withdrawal(tx: u32, amount: i64) -> Row {
        Row::Withdrawal {
            client_id: ClientId(1),
            tx_id: TxId(tx),
            amount: Decimal::from(amount),
        }
    }

    fn dispute(tx: u32) -> Row {
        Row::Dispute {
            client_id: ClientId(1),
            tx_id: TxId(tx),
        }
    }

    fn resolve(tx: u32) -> Row {
        Row::Resolve {
            client_id: ClientId(1),
            tx_id: TxId(tx),
        }
    }

    fn chargeback(tx: u32) -> Row {
        Row::Chargeback {
            client_id: ClientId(1),
            tx_id: TxId(tx),
        }
    }

    fn assert_balances(account: &ClientAccount, available: i64, held: i64, locked: bool) {
        assert_eq!(account.balances.available, Decimal::from(available));
        assert_eq!(account.balances.held, Decimal::from(held));
        assert_eq!(account.balances.locked, locked);
    }

    #[test]
    fn withdrawing_all_available_funds_is_allowed() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        account.process_row(deposit(1, 10))?;
        account.process_row(withdrawal(2, 10))?;
        assert_balances(&account, 0, 0, false);
        assert_eq!(account.txs.len(), 2);
        Ok(())
    }

    #[test]
    fn insufficient_funds_do_not_change_balances_or_create_a_transaction() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        account.process_row(deposit(1, 10))?;
        account.process_row(dispute(1))?;
        assert_eq!(
            account.process_row(withdrawal(2, 1)),
            Err(RejectionReason::InsufficientFunds {
                available: Decimal::ZERO,
                requested: Decimal::ONE,
            })
        );
        assert_balances(&account, 0, 10, false);
        assert!(!account.txs.contains_key(&TxId(2)));
        account.process_row(dispute(2))?;
        assert_balances(&account, 0, 10, false);
        Ok(())
    }

    #[test]
    fn dispute_and_resolve_transfer_funds_once() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        account.process_row(deposit(1, 10))?;
        account.process_row(dispute(1))?;
        account.process_row(dispute(1))?;
        assert_balances(&account, 0, 10, false);
        assert_eq!(account.txs[&TxId(1)].state, ClientTxState::Disputed);
        account.process_row(resolve(1))?;
        account.process_row(resolve(1))?;
        assert_balances(&account, 10, 0, false);
        assert_eq!(account.txs[&TxId(1)].state, ClientTxState::Resolved);
        Ok(())
    }

    #[test]
    fn resolved_transactions_are_not_disputed_or_charged_back_again() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        for row in [
            deposit(1, 10),
            dispute(1),
            resolve(1),
            dispute(1),
            chargeback(1),
        ] {
            account.process_row(row)?;
        }
        assert_balances(&account, 10, 0, false);
        Ok(())
    }

    #[test]
    fn chargeback_removes_held_funds_and_freezes_the_account() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        for row in [deposit(1, 10), deposit(2, 20), dispute(1), chargeback(1)] {
            account.process_row(row)?;
        }
        assert_balances(&account, 20, 0, true);
        assert_eq!(account.txs[&TxId(1)].state, ClientTxState::ChargedBack);
        assert_eq!(
            account.process_row(chargeback(1)),
            Err(RejectionReason::AccountLocked)
        );
        assert_balances(&account, 20, 0, true);
        Ok(())
    }

    #[test]
    fn all_operations_are_rejected_on_a_frozen_account() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        for row in [
            deposit(1, 10),
            deposit(2, 20),
            deposit(3, 30),
            dispute(1),
            dispute(2),
            chargeback(1),
        ] {
            account.process_row(row)?;
        }
        for row in [
            deposit(4, 5),
            withdrawal(5, 5),
            dispute(3),
            resolve(2),
            chargeback(2),
        ] {
            assert_eq!(
                account.process_row(row),
                Err(RejectionReason::AccountLocked)
            );
            assert_balances(&account, 30, 20, true);
            assert_eq!(account.txs.len(), 3);
            assert_eq!(account.txs[&TxId(2)].state, ClientTxState::Disputed);
            assert_eq!(account.txs[&TxId(3)].state, ClientTxState::Accounted);
        }
        Ok(())
    }

    #[test]
    fn unknown_references_and_invalid_states_are_ignored() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        for row in [
            deposit(1, 10),
            dispute(99),
            resolve(99),
            chargeback(99),
            resolve(1),
            chargeback(1),
        ] {
            account.process_row(row)?;
        }
        assert_balances(&account, 10, 0, false);
        assert_eq!(account.txs[&TxId(1)].state, ClientTxState::Accounted);
        Ok(())
    }

    #[test]
    fn spent_deposits_can_still_be_disputed() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        for row in [deposit(1, 10), withdrawal(2, 10), dispute(1)] {
            account.process_row(row)?;
        }
        assert_balances(&account, -10, 10, false);
        account.process_row(chargeback(1))?;
        assert_balances(&account, -10, 0, true);
        Ok(())
    }

    #[test]
    fn withdrawal_disputes_follow_the_same_balance_rules() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        for row in [deposit(1, 10), withdrawal(2, 3), dispute(2)] {
            account.process_row(row)?;
        }
        assert_balances(&account, 4, 3, false);
        account.process_row(chargeback(2))?;
        assert_balances(&account, 4, 0, true);
        Ok(())
    }

    #[test]
    fn invalid_amounts_are_rejected_without_mutation() {
        let mut account = ClientAccount::default();
        for amount in [Decimal::NEGATIVE_ONE, Decimal::new(1, 5)] {
            for row in [
                Row::Deposit {
                    client_id: ClientId(1),
                    tx_id: TxId(1),
                    amount,
                },
                Row::Withdrawal {
                    client_id: ClientId(1),
                    tx_id: TxId(1),
                    amount,
                },
            ] {
                assert_eq!(
                    account.process_row(row),
                    Err(RejectionReason::InvalidAmount { amount })
                );
                assert_balances(&account, 0, 0, false);
                assert!(account.txs.is_empty());
            }
        }
    }

    #[test]
    fn deposit_overflow_is_rejected_atomically() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        account.process_row(Row::Deposit {
            client_id: ClientId(1),
            tx_id: TxId(1),
            amount: Decimal::MAX,
        })?;
        assert_eq!(
            account.process_row(deposit(2, 1)),
            Err(RejectionReason::BalanceOverflow)
        );
        assert_eq!(account.balances.available, Decimal::MAX);
        assert!(!account.txs.contains_key(&TxId(2)));
        account.process_row(dispute(1))?;
        assert_eq!(
            account.process_row(deposit(2, 1)),
            Err(RejectionReason::BalanceOverflow)
        );
        assert_eq!(account.balances.available, Decimal::ZERO);
        assert_eq!(account.balances.held, Decimal::MAX);
        Ok(())
    }

    #[test]
    fn dispute_overflow_leaves_both_balances_and_state_unchanged() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        for row in [
            Row::Deposit {
                client_id: ClientId(1),
                tx_id: TxId(1),
                amount: Decimal::MAX,
            },
            Row::Withdrawal {
                client_id: ClientId(1),
                tx_id: TxId(2),
                amount: Decimal::MAX,
            },
            dispute(1),
        ] {
            account.process_row(row)?;
        }
        assert_eq!(
            account.process_row(dispute(2)),
            Err(RejectionReason::BalanceOverflow)
        );
        assert_eq!(account.balances.available, -Decimal::MAX);
        assert_eq!(account.balances.held, Decimal::MAX);
        assert_eq!(account.txs[&TxId(2)].state, ClientTxState::Accounted);
        Ok(())
    }

    #[test]
    fn amounts_must_not_be_silently_rounded_at_large_balances() -> eyre::Result<()> {
        let mut account = ClientAccount::default();
        account.process_row(Row::Deposit {
            client_id: ClientId(1),
            tx_id: TxId(1),
            amount: Decimal::MAX,
        })?;
        for row in [
            Row::Deposit {
                client_id: ClientId(1),
                tx_id: TxId(2),
                amount: Decimal::new(1, 4),
            },
            Row::Withdrawal {
                client_id: ClientId(1),
                tx_id: TxId(2),
                amount: Decimal::new(1, 4),
            },
        ] {
            assert_eq!(
                account.process_row(row),
                Err(RejectionReason::BalanceOverflow)
            );
            assert_eq!(account.balances.available, Decimal::MAX);
            assert!(!account.txs.contains_key(&TxId(2)));
        }
        Ok(())
    }

    #[test]
    fn dispute_completion_must_preserve_small_amounts_at_large_balances() -> eyre::Result<()> {
        for action in [resolve(1), chargeback(1)] {
            let mut account = ClientAccount::default();
            for (tx, amount) in [(1, Decimal::new(1, 4)), (2, Decimal::new(9999, 4))] {
                account.process_row(Row::Deposit {
                    client_id: ClientId(1),
                    tx_id: TxId(tx),
                    amount,
                })?;
                account.process_row(dispute(tx))?;
            }
            account.process_row(Row::Deposit {
                client_id: ClientId(1),
                tx_id: TxId(3),
                amount: Decimal::MAX - Decimal::ONE,
            })?;
            assert_eq!(
                account.process_row(action),
                Err(RejectionReason::BalanceOverflow)
            );
            assert_eq!(account.balances.available, Decimal::MAX - Decimal::ONE);
            assert_eq!(account.balances.held, Decimal::ONE);
            assert!(!account.balances.locked);
            assert_eq!(account.txs[&TxId(1)].state, ClientTxState::Disputed);
        }
        Ok(())
    }

    #[test]
    fn rejections_include_ids_and_a_structured_cause() {
        use std::error::Error;

        let mut processor = TransactionsProcessor::default();
        let Err(error) = processor.process_row(withdrawal(23, 5)) else {
            panic!("withdrawal from an empty account should fail");
        };
        assert_eq!(error.client_id, ClientId(1));
        assert_eq!(error.tx_id, TxId(23));
        assert!(error.source().is_some());
        assert!(matches!(
            error.reason,
            RejectionReason::InsufficientFunds { .. }
        ));
    }

    #[test]
    fn unknown_clients_are_not_created_by_dispute_operations() -> eyre::Result<()> {
        let mut processor = TransactionsProcessor::default();
        for row in [dispute(1), resolve(1), chargeback(1)] {
            processor.process_row(row)?;
        }
        assert_eq!(processor.into_output().count(), 0);
        Ok(())
    }

    #[test]
    fn transaction_references_cannot_cross_client_accounts() -> eyre::Result<()> {
        let mut processor = TransactionsProcessor::default();
        processor.process_row(deposit(1, 10))?;
        processor.process_row(Row::Deposit {
            client_id: ClientId(2),
            tx_id: TxId(2),
            amount: Decimal::from(20),
        })?;
        for row in [
            Row::Dispute {
                client_id: ClientId(2),
                tx_id: TxId(1),
            },
            Row::Resolve {
                client_id: ClientId(2),
                tx_id: TxId(1),
            },
            Row::Chargeback {
                client_id: ClientId(2),
                tx_id: TxId(1),
            },
        ] {
            processor.process_row(row)?;
        }
        for output in processor.into_output() {
            assert_eq!(
                output.available,
                Decimal::from(u32::from(output.client_id.0) * 10)
            );
            assert_eq!(output.held, Decimal::ZERO);
            assert_eq!(output.total, output.available + output.held);
            assert!(!output.locked);
        }
        Ok(())
    }
}
