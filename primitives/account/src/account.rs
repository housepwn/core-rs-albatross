use beserial::{Deserialize, ReadBytesExt, Serialize, SerializingError, WriteBytesExt};
use nimiq_primitives::account::AccountType;
use nimiq_primitives::coin::Coin;
use nimiq_transaction::Transaction;

use crate::interaction_traits::{AccountInherentInteraction, AccountTransactionInteraction};
use crate::{
    AccountError, AccountsTree, BasicAccount, HashedTimeLockedContract, Inherent, StakingContract,
    VestingContract,
};
use nimiq_database::WriteTransaction;
use nimiq_trie::key_nibbles::KeyNibbles;

#[derive(Clone, PartialEq, PartialOrd, Eq, Ord, Debug)]
#[cfg_attr(feature = "serde-derive", derive(serde::Serialize, serde::Deserialize))]
pub enum Account {
    Basic(BasicAccount),
    Vesting(VestingContract),
    HTLC(HashedTimeLockedContract),
    #[cfg_attr(feature = "serde-derive", serde(skip))]
    Staking(StakingContract),
}

impl Account {
    pub fn account_type(&self) -> AccountType {
        match *self {
            Account::Basic(_) => AccountType::Basic,
            Account::Vesting(_) => AccountType::Vesting,
            Account::HTLC(_) => AccountType::HTLC,
            Account::Staking(_) => AccountType::Staking,
        }
    }

    pub fn balance(&self) -> Coin {
        match *self {
            Account::Basic(ref account) => account.balance,
            Account::Vesting(ref account) => account.balance,
            Account::HTLC(ref account) => account.balance,
            Account::Staking(ref account) => account.balance,
        }
    }

    pub fn balance_add(balance: Coin, value: Coin) -> Result<Coin, AccountError> {
        balance
            .checked_add(value)
            .ok_or(AccountError::InvalidCoinValue)
    }

    pub fn balance_sub(balance: Coin, value: Coin) -> Result<Coin, AccountError> {
        match balance.checked_sub(value) {
            Some(result) => Ok(result),
            None => Err(AccountError::InsufficientFunds {
                balance,
                needed: value,
            }),
        }
    }

    pub fn balance_sufficient(balance: Coin, value: Coin) -> Result<(), AccountError> {
        if balance < value {
            Err(AccountError::InsufficientFunds {
                balance,
                needed: value,
            })
        } else {
            Ok(())
        }
    }
}

impl AccountTransactionInteraction for Account {
    fn create(
        accounts_tree: &AccountsTree,
        db_txn: &mut WriteTransaction,
        balance: Coin,
        transaction: &Transaction,
        block_height: u32,
        block_time: u64,
    ) -> Result<(), AccountError> {
        match transaction.recipient_type {
            AccountType::Basic => Err(AccountError::InvalidForRecipient),
            AccountType::Vesting => VestingContract::create(
                accounts_tree,
                db_txn,
                balance,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::HTLC => HashedTimeLockedContract::create(
                accounts_tree,
                db_txn,
                balance,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::Staking => Err(AccountError::InvalidForRecipient),
        }
    }

    fn commit_incoming_transaction(
        accounts_tree: &AccountsTree,
        db_txn: &mut WriteTransaction,
        transaction: &Transaction,
        block_height: u32,
        block_time: u64,
    ) -> Result<Option<Vec<u8>>, AccountError> {
        match transaction.recipient_type {
            AccountType::Basic => BasicAccount::commit_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::Vesting => VestingContract::commit_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::HTLC => HashedTimeLockedContract::commit_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::Staking => StakingContract::commit_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
        }
    }

    fn revert_incoming_transaction(
        accounts_tree: &AccountsTree,
        db_txn: &mut WriteTransaction,
        transaction: &Transaction,
        block_height: u32,
        block_time: u64,
        receipt: Option<&Vec<u8>>,
    ) -> Result<(), AccountError> {
        match transaction.recipient_type {
            AccountType::Basic => BasicAccount::revert_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::Vesting => VestingContract::revert_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::HTLC => HashedTimeLockedContract::revert_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::Staking => StakingContract::revert_incoming_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
        }
    }

    fn commit_outgoing_transaction(
        accounts_tree: &AccountsTree,
        db_txn: &mut WriteTransaction,
        transaction: &Transaction,
        block_height: u32,
        block_time: u64,
    ) -> Result<Option<Vec<u8>>, AccountError> {
        match transaction.sender_type {
            AccountType::Basic => BasicAccount::commit_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::Vesting => VestingContract::commit_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::HTLC => HashedTimeLockedContract::commit_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
            AccountType::Staking => StakingContract::commit_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
            ),
        }
    }

    fn revert_outgoing_transaction(
        accounts_tree: &AccountsTree,
        db_txn: &mut WriteTransaction,
        transaction: &Transaction,
        block_height: u32,
        block_time: u64,
        receipt: Option<&Vec<u8>>,
    ) -> Result<(), AccountError> {
        match transaction.sender_type {
            AccountType::Basic => BasicAccount::revert_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::Vesting => VestingContract::revert_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::HTLC => HashedTimeLockedContract::revert_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::Staking => StakingContract::revert_outgoing_transaction(
                accounts_tree,
                db_txn,
                transaction,
                block_height,
                block_time,
                receipt,
            ),
        }
    }
}

impl AccountInherentInteraction for Account {
    fn commit_inherent(
        accounts_tree: &AccountsTree,
        db_txn: &mut WriteTransaction,
        inherent: &Inherent,
        block_height: u32,
        block_time: u64,
    ) -> Result<Option<Vec<u8>>, AccountError> {
        match inherent.target_type {
            AccountType::Basic => BasicAccount::commit_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
            ),
            AccountType::Vesting => VestingContract::commit_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
            ),
            AccountType::HTLC => HashedTimeLockedContract::commit_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
            ),
            AccountType::Staking => StakingContract::commit_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
            ),
        }
    }

    fn revert_inherent(
        accounts_tree: &AccountsTree,
        db_txn: &mut WriteTransaction,
        inherent: &Inherent,
        block_height: u32,
        block_time: u64,
        receipt: Option<&Vec<u8>>,
    ) -> Result<(), AccountError> {
        match inherent.target_type {
            AccountType::Basic => BasicAccount::revert_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::Vesting => VestingContract::revert_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::HTLC => HashedTimeLockedContract::revert_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
                receipt,
            ),
            AccountType::Staking => StakingContract::revert_inherent(
                accounts_tree,
                db_txn,
                inherent,
                block_height,
                block_time,
                receipt,
            ),
        }
    }
}

impl Serialize for Account {
    fn serialize<W: WriteBytesExt>(&self, writer: &mut W) -> Result<usize, SerializingError> {
        let mut size: usize = 0;
        size += Serialize::serialize(&self.account_type(), writer)?;

        match *self {
            Account::Basic(ref account) => {
                size += Serialize::serialize(&account, writer)?;
            }
            Account::Vesting(ref account) => {
                size += Serialize::serialize(&account, writer)?;
            }
            Account::HTLC(ref account) => {
                size += Serialize::serialize(&account, writer)?;
            }
            Account::Staking(ref account) => {
                size += Serialize::serialize(&account, writer)?;
            }
        }

        Ok(size)
    }

    fn serialized_size(&self) -> usize {
        let mut size = /*type*/ 1;

        match *self {
            Account::Basic(ref account) => {
                size += Serialize::serialized_size(&account);
            }
            Account::Vesting(ref account) => {
                size += Serialize::serialized_size(&account);
            }
            Account::HTLC(ref account) => {
                size += Serialize::serialized_size(&account);
            }
            Account::Staking(ref account) => {
                size += Serialize::serialized_size(&account);
            }
        }

        size
    }
}

impl Deserialize for Account {
    fn deserialize<R: ReadBytesExt>(reader: &mut R) -> Result<Self, SerializingError> {
        let account_type: AccountType = Deserialize::deserialize(reader)?;

        match account_type {
            AccountType::Basic => {
                let account: BasicAccount = Deserialize::deserialize(reader)?;
                Ok(Account::Basic(account))
            }
            AccountType::Vesting => {
                let account: VestingContract = Deserialize::deserialize(reader)?;
                Ok(Account::Vesting(account))
            }
            AccountType::HTLC => {
                let account: HashedTimeLockedContract = Deserialize::deserialize(reader)?;
                Ok(Account::HTLC(account))
            }
            AccountType::Staking => {
                let account: StakingContract = Deserialize::deserialize(reader)?;
                Ok(Account::Staking(account))
            }
            AccountType::Reward => unimplemented!(),
        }
    }
}
