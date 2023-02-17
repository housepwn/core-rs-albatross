use nimiq_primitives::account::AccountType;
use nimiq_primitives::{account::AccountError, coin::Coin};
use nimiq_transaction::{inherent::Inherent, Transaction};

use crate::data_store::{DataStoreRead, DataStoreWrite};
use crate::{Account, AccountReceipt, Inherent};

pub struct BlockState {
    pub number: u32,
    pub time: u64,
}

impl BlockState {
    pub fn new(block_number: u32, block_time: u64) -> Self {
        Self {
            number: block_number,
            time: block_time,
        }
    }
}

pub trait AccountTransactionInteraction: Sized {
    fn create_new_contract(
        transaction: &Transaction,
        initial_balance: Coin,
        block_state: &BlockState,
        data_store: DataStoreWrite,
    ) -> Result<Account, AccountError>;

    fn revert_new_contract(
        &mut self,
        transaction: &Transaction,
        block_state: &BlockState,
        data_store: DataStoreWrite,
    ) -> Result<(), AccountError>;

    fn commit_incoming_transaction(
        &mut self,
        transaction: &Transaction,
        block_state: &BlockState,
        data_store: DataStoreWrite,
    ) -> Result<Option<AccountReceipt>, AccountError>;

    fn revert_incoming_transaction(
        &mut self,
        transaction: &Transaction,
        block_state: &BlockState,
        receipt: Option<AccountReceipt>,
        data_store: DataStoreWrite,
    ) -> Result<(), AccountError>;

    fn commit_outgoing_transaction(
        &mut self,
        transaction: &Transaction,
        block_state: &BlockState,
        data_store: DataStoreWrite,
    ) -> Result<Option<AccountReceipt>, AccountError>;

    fn revert_outgoing_transaction(
        &mut self,
        transaction: &Transaction,
        block_state: &BlockState,
        receipt: Option<AccountReceipt>,
        data_store: DataStoreWrite,
    ) -> Result<(), AccountError>;

    fn commit_failed_transaction(
        &mut self,
        transaction: &Transaction,
        block_state: &BlockState,
        data_store: DataStoreWrite,
    ) -> Result<Option<AccountReceipt>, AccountError>;

    fn revert_failed_transaction(
        &mut self,
        transaction: &Transaction,
        block_state: &BlockState,
        receipt: Option<AccountReceipt>,
        data_store: DataStoreWrite,
    ) -> Result<(), AccountError>;

    fn has_sufficient_balance(
        &self,
        transaction: &Transaction,
        reserved_balance: Coin,
        block_state: &BlockState,
        data_store: DataStoreRead,
    ) -> Result<bool, AccountError>;
}

pub trait AccountInherentInteraction: Sized {
    fn commit_inherent(
        &mut self,
        inherent: &Inherent,
        block_state: &BlockState,
        data_store: DataStoreWrite,
    ) -> Result<Option<AccountReceipt>, AccountError>;

    fn revert_inherent(
        &mut self,
        inherent: &Inherent,
        block_state: &BlockState,
        receipt: Option<AccountReceipt>,
        data_store: DataStoreWrite,
    ) -> Result<(), AccountError>;
}

pub trait AccountPruningInteraction: Sized {
    fn can_be_pruned(&self) -> bool;

    fn prune(self, data_store: DataStoreRead) -> Result<Option<AccountReceipt>, AccountError>;

    fn restore(
        ty: AccountType,
        pruned_account: Option<&AccountReceipt>,
        data_store: DataStoreWrite,
    ) -> Result<Account, AccountError>;
}
