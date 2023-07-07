use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use nimiq_collections::BitSet;
use nimiq_keys::Address;
use nimiq_primitives::{
    coin::Coin,
    policy::Policy,
    slots_allocation::{PenalizedSlot, SlashedValidator},
};
use nimiq_serde::{Deserialize, Serialize};

/// Data structure to keep track of the punished slots of the previous and current batch.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishedSlots {
    // The validator slots that lost rewards (i.e. are not eligible to receive rewards) during
    // the current epoch.
    current_batch_punished_slots: BTreeMap<Address, BTreeSet<u16>>,
    // The validator slots that lost rewards (i.e. are not eligible to receive rewards) during
    // the previous batch.
    previous_batch_punished_slots: BitSet,
}

impl PunishedSlots {
    /// Registers a new slash for a given validator.
    /// The slash always affects the batch in which the event happened.
    /// If the event was only reported in the subsequent batch, it will affect both sets.
    /// In the case the validator is not elected, the current set remains unchanged.
    pub fn register_slash(
        &mut self,
        slashed_validator: &SlashedValidator,
        reporting_block: u32,
        new_epoch_slot_range: &Option<Range<u16>>,
    ) -> (BitSet, Option<BTreeSet<u16>>) {
        let old_previous_batch_punished_slots = self.previous_batch_punished_slots.clone();
        let old_current_batch_punished_slots = self
            .current_batch_punished_slots
            .get(&slashed_validator.validator_address)
            .cloned();

        if Policy::batch_at(slashed_validator.event_block) == Policy::batch_at(reporting_block) {
            let entry = self
                .current_batch_punished_slots
                .entry(slashed_validator.validator_address.clone())
                .or_insert_with(BTreeSet::new);

            for slot in slashed_validator.slots.clone() {
                entry.insert(slot);
            }
        } else if Policy::batch_at(slashed_validator.event_block) + 1
            == Policy::batch_at(reporting_block)
        {
            // Reported during subsequent batch, so changes the previous set.
            for slot in slashed_validator.slots.clone() {
                self.previous_batch_punished_slots.insert(slot as usize);
            }

            // If we are in the current epoch, the slot range will be the same as slot_range.
            // Otherwise, we must use the slots of the new epoch to slash.
            // If the validator is wasn't elected for the new epoch, then we don't slash.
            if let Some(ref slots) = new_epoch_slot_range {
                let entry = self
                    .current_batch_punished_slots
                    .entry(slashed_validator.validator_address.clone())
                    .or_insert_with(BTreeSet::new);

                for slot in slots.clone() {
                    entry.insert(slot);
                }
            }
        }

        (
            old_previous_batch_punished_slots,
            old_current_batch_punished_slots,
        )
    }

    /// Reverts a new penalty for a given slot.
    pub fn revert_register_slash(
        &mut self,
        slashed_validator: &SlashedValidator,
        old_previous_batch_punished_slots: BitSet,
        old_current_batch_punished_slots: Option<BTreeSet<u16>>,
    ) {
        self.previous_batch_punished_slots = old_previous_batch_punished_slots;
        if let Some(set) = old_current_batch_punished_slots {
            assert!(
                self.current_batch_punished_slots
                    .insert(slashed_validator.validator_address.clone(), set)
                    .is_some(),
                "Missing slashed validator"
            );
        } else {
            assert!(
                self.current_batch_punished_slots
                    .remove(&slashed_validator.validator_address)
                    .is_some(),
                "Missing slashed validator"
            );
        }
    }

    /// Registers a new penalty for a given slot.
    /// The penalty always affects the batch in which the event happened.
    /// If the event was only reported in the subsequent batch, but in the same epoch,
    /// it will affect the current batch too.
    pub fn register_penalty(
        &mut self,
        penalized_slot: &PenalizedSlot,
        reporting_block: u32,
    ) -> (bool, bool) {
        let newly_punished_previous_batch = !self
            .previous_batch_punished_slots
            .contains(penalized_slot.slot as usize);
        let mut newly_punished_current_batch = false;

        // Reported during subsequent batch, so changes the previous set.
        if Policy::batch_at(penalized_slot.event_block) + 1 == Policy::batch_at(reporting_block) {
            self.previous_batch_punished_slots
                .insert(penalized_slot.slot as usize);
        }
        // Only apply the penalty to the current set if the epoch is the same.
        // On a new epoch the validator may not be elected. Even if it is,
        // there is no straightforward mapping between the slots of the previous and new epoch.
        if Policy::epoch_at(penalized_slot.event_block) == Policy::epoch_at(reporting_block) {
            newly_punished_current_batch = self
                .current_batch_punished_slots
                .entry(penalized_slot.validator_address.clone())
                .or_insert_with(BTreeSet::new)
                .insert(penalized_slot.slot);
        }

        (newly_punished_previous_batch, newly_punished_current_batch)
    }

    /// Reverts a new penalty for a given slot.
    pub fn revert_register_penalty(
        &mut self,
        penalized_slot: &PenalizedSlot,
        newly_punished_previous_batch: bool,
        newly_punished_current_batch: bool,
    ) {
        if newly_punished_previous_batch {
            self.previous_batch_punished_slots
                .remove(penalized_slot.slot as usize);
        }

        if newly_punished_current_batch {
            let entry = self
                .current_batch_punished_slots
                .get_mut(&penalized_slot.validator_address)
                .expect("Missing validator");

            assert!(
                entry.remove(&penalized_slot.slot),
                "Should have penalized slot"
            );

            if entry.is_empty() {
                self.current_batch_punished_slots
                    .remove(&penalized_slot.validator_address);
            }
        }
    }

    /// At the end of a batch, we update the previous bitset and remove reactivated validators from the current bitset.
    pub fn finalize_batch(&mut self, current_active_validators: &BTreeMap<Address, Coin>) {
        // Updates the previous bitset with the current one.
        self.previous_batch_punished_slots = self.current_batch_punished_slots();

        // Remove all validators that are active again.
        self.current_batch_punished_slots
            .retain(|validator_address, _| {
                current_active_validators.get(validator_address).is_none()
            });
    }

    // At the end of an epoch the current bitset is reset and the previous bitset
    // should retain the information of the last batch.
    pub fn finalize_epoch(&mut self) {
        // Updates the previous bitset with the current one.
        self.previous_batch_punished_slots = self.current_batch_punished_slots();

        // At an epoch boundary, the next starting set is empty.
        self.current_batch_punished_slots = Default::default();
    }

    /// Returns a BitSet of slots that were punished in the current epoch.
    pub fn current_batch_punished_slots(&self) -> BitSet {
        let mut bitset = BitSet::new();
        for slots in self.current_batch_punished_slots.values() {
            for &slot in slots {
                bitset.insert(slot as usize);
            }
        }
        bitset
    }

    /// Returns a BitSet of slots that were punished in the previous epoch.
    pub fn previous_batch_punished_slots(&self) -> &BitSet {
        &self.previous_batch_punished_slots
    }
}
