use beserial::{Serialize, Deserialize};
use consensus::base::transaction::Transaction;
use super::{Account, AccountError};
use consensus::base::transaction::SignatureProof;
use consensus::base::primitive::Address;
use consensus::base::primitive::hash::{Hasher, Blake2bHash, Blake2bHasher, HashAlgorithm};
use std::io;

#[derive(Clone, PartialEq, PartialOrd, Eq, Ord, Debug, Serialize, Deserialize)]
pub struct HashedTimeLockedContract {
    pub balance: u64,
    pub sender: Address,
    pub recipient: Address,
    pub hash_root: Blake2bHash, // TODO add support for other hash algorithms
    pub hash_count: u8,
    pub timeout: u32,
    pub total_amount: u64
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum ProofType {
    RegularTransfer = 1,
    EarlyResolve = 2,
    TimeoutResolve = 3
}

impl HashedTimeLockedContract {
    pub fn create(balance: u64, transaction: &Transaction, block_height: u32) -> Result<Self, AccountError> {
        return match HashedTimeLockedContract::create_from_transaction(balance, transaction) {
            Ok(account) => Ok(account),
            Err(_) => Err(AccountError("Failed to create HTLC".to_string()))
        };
    }

    fn create_from_transaction(balance: u64, transaction: &Transaction) -> io::Result<Self> {
        let reader = &mut &transaction.data[..];

        let sender: Address = Deserialize::deserialize(reader)?;
        let recipient: Address = Deserialize::deserialize(reader)?;
        let hash_algorithm: HashAlgorithm = Deserialize::deserialize(reader)?;
        let hash_root = Deserialize::deserialize(reader)?;
        let hash_count = Deserialize::deserialize(reader)?;
        let timeout = Deserialize::deserialize(reader)?;
        let total_amount = Deserialize::deserialize(reader)?;

        if hash_count == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid hash_count"));
        }

        return Ok(HashedTimeLockedContract::new(transaction.value, sender, recipient, hash_root, hash_count, timeout, total_amount));
    }

    fn new(balance: u64, sender: Address, recipient: Address, hash_root: Blake2bHash, hash_count: u8, timeout: u32, total_amount: u64) -> Self {
        return HashedTimeLockedContract { balance, sender, recipient, hash_root, hash_count, timeout, total_amount };
    }

    pub fn verify_incoming_transaction(transaction: &Transaction) -> bool {
        // The contract creation transaction is the only valid incoming transaction.
        if transaction.recipient != transaction.contract_creation_address() {
            return false;
        }

        // TODO verify create arguments

        return true;
    }

    pub fn verify_outgoing_transaction(transaction: &Transaction) -> bool {
        let verify = || -> io::Result<bool> {
            let tx_content = transaction.serialize_content();
            let tx_buf = tx_content.as_slice();

            let proof_buf = &mut &transaction.proof[..];
            let proof_type: ProofType = Deserialize::deserialize(proof_buf)?;
            match proof_type {
                ProofType::RegularTransfer => {
                    let hash_algorithm: HashAlgorithm = Deserialize::deserialize(proof_buf)?;
                    let hash_depth: u8 = Deserialize::deserialize(proof_buf)?;
                    let hash_root: Blake2bHash = Deserialize::deserialize(proof_buf)?;
                    let mut pre_image: Blake2bHash = Deserialize::deserialize(proof_buf)?;

                    for i in 0..hash_depth {
                        match hash_algorithm {
                            HashAlgorithm::Blake2b => {
                                pre_image = Blake2bHasher::default().digest(pre_image.as_bytes());
                            },
                            // TODO add support for other hash algorithms
                            _ => unimplemented!()
                        }
                    }

                    if hash_root != pre_image {
                        return Ok(false);
                    }

                    let signature_proof: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    return Ok(signature_proof.verify(tx_buf));
                },
                ProofType::EarlyResolve => {
                    let signature_proof_recipient: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    let signature_proof_sender: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    return Ok(
                        signature_proof_recipient.verify(tx_buf)
                        && signature_proof_sender.verify(tx_buf));
                },
                ProofType::TimeoutResolve => {
                    let signature_proof: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    return Ok(signature_proof.verify(tx_buf));
                }
            }
        };

        // TODO reject overlong proofs

        return match verify() {
            Ok(result) => result,
            Err(e) => false
        };
    }

    fn with_balance(&self, balance: u64) -> Self {
        return HashedTimeLockedContract {
            balance,
            sender: self.sender.clone(),
            recipient: self.recipient.clone(),
            hash_root: self.hash_root.clone(),
            hash_count: self.hash_count,
            timeout: self.timeout,
            total_amount: self.total_amount
        };
    }

    pub fn with_incoming_transaction(&self, transaction: &Transaction, block_height: u32) -> Result<Self, AccountError> {
        return Err(AccountError("Illegal incoming transaction".to_string()));
    }

    pub fn without_incoming_transaction(&self, transaction: &Transaction, block_height: u32) -> Result<Self, AccountError> {
        return Err(AccountError("Illegal incoming transaction".to_string()));
    }

    pub fn with_outgoing_transaction(&self, transaction: &Transaction, block_height: u32) -> Result<Self, AccountError> {
        let balance: u64 = Account::balance_sub(self.balance, transaction.value + transaction.fee)?;

        let verify = || -> io::Result<bool> {
            let proof_buf = &mut &transaction.proof[..];
            let proof_type: ProofType = Deserialize::deserialize(proof_buf)?;
            match proof_type {
                ProofType::RegularTransfer => {
                    // Check that the contract has not expired yet.
                    if self.timeout < block_height {
                        return Ok(false);
                    }

                    // Check that the provided hash_root is correct.
                    let hash_algorithm: HashAlgorithm = Deserialize::deserialize(proof_buf)?;
                    let hash_depth: u8 = Deserialize::deserialize(proof_buf)?;
                    let hash_root: Blake2bHash = Deserialize::deserialize(proof_buf)?;
                    if hash_root != self.hash_root {
                        return Ok(false);
                    }

                    // Ignore pre_image.
                    let pre_image: Blake2bHash = Deserialize::deserialize(proof_buf)?;

                    // Check that the transaction is signed by the authorized recipient.
                    let signature_proof: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    if !signature_proof.is_signed_by(&self.recipient) {
                        return Ok(false);
                    }

                    // Check min cap.
                    let cap_ratio = 1f64 - (hash_depth as f64 / self.hash_count as f64);
                    let min_cap = (cap_ratio * self.total_amount as f64).floor().max(0f64) as u64;
                    return Ok(balance >= min_cap);
                },
                ProofType::EarlyResolve => {
                    let signature_proof_recipient: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    let signature_proof_sender: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    return Ok(
                        signature_proof_recipient.is_signed_by(&self.recipient)
                        && signature_proof_sender.is_signed_by(&self.sender));
                },
                ProofType::TimeoutResolve => {
                    let signature_proof: SignatureProof = Deserialize::deserialize(proof_buf)?;
                    return Ok(signature_proof.is_signed_by(&self.sender));
                }
            }
        };

        return match verify() {
            Ok(true) => Ok(self.with_balance(balance)),
            _ => Err(AccountError("Proof error".to_string()))
        };
    }

    pub fn without_outgoing_transaction(&self, transaction: &Transaction, block_height: u32) -> Result<Self, AccountError> {
        let balance: u64 = Account::balance_add(self.balance, transaction.value + transaction.fee)?;
        return Ok(self.with_balance(balance));
    }
}
