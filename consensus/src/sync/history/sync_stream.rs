use std::pin::Pin;
use std::sync::Arc;

use futures::stream::Stream;
use futures::task::{Context, Poll};
use futures::{FutureExt, StreamExt};
use tokio::task::spawn_blocking;

use nimiq_block::Block;
use nimiq_blockchain::Blockchain;
use nimiq_network_interface::prelude::{Network, NetworkEvent, Peer};

use crate::sync::history::cluster::{SyncCluster, SyncClusterResult};
use crate::sync::history::sync::{HistorySyncReturn, Job};
use crate::sync::history::HistorySync;
use crate::sync::request_component::HistorySyncStream;

impl<TNetwork: Network> HistorySync<TNetwork> {
    fn poll_network_events(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<HistorySyncReturn<TNetwork::PeerType>>> {
        while let Poll::Ready(Some(result)) = self.network_event_rx.poll_next_unpin(cx) {
            match result {
                Ok(NetworkEvent::PeerLeft(peer)) => {
                    // Delete the ConsensusAgent from the agents map, removing the only "persistent"
                    // strong reference to it. There might not be an entry for every peer (e.g. if
                    // it didn't send any epoch ids).
                    self.remove_peer(peer.id());
                    self.peers.remove(&peer.id());
                }
                Ok(NetworkEvent::PeerJoined(peer)) => {
                    // Create a ConsensusAgent for the peer that joined and request epoch_ids from it.
                    self.add_peer(peer.id());
                }
                Err(_) => return Poll::Ready(None),
            }
        }

        Poll::Pending
    }

    fn poll_epoch_ids(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<HistorySyncReturn<TNetwork::PeerType>>> {
        // TODO We might want to not send an epoch_id request in the first place if we're at the
        //  cluster limit.
        while self.epoch_clusters.len() < Self::MAX_CLUSTERS {
            let epoch_ids = match self.epoch_ids_stream.poll_next_unpin(cx) {
                Poll::Ready(Some(epoch_ids)) => epoch_ids,
                _ => break,
            };

            if let Some(epoch_ids) = epoch_ids {
                // The peer might have disconnected during the request.
                // FIXME Check if the peer is still connected

                // If the peer didn't find any of our locators, we are done with it and emit it.
                if !epoch_ids.locator_found {
                    debug!(
                        "Peer is behind or on different chain: {:?}",
                        epoch_ids.sender
                    );
                    return Poll::Ready(Some(HistorySyncReturn::Outdated(epoch_ids.sender)));
                } else if epoch_ids.ids.is_empty() && epoch_ids.checkpoint_id.is_none() {
                    // We are synced with this peer.
                    debug!("Finished syncing with peer: {:?}", epoch_ids.sender);
                    return Poll::Ready(Some(HistorySyncReturn::Good(epoch_ids.sender)));
                }

                // If the clustering deems a peer useless, it is returned here and we emit it.
                if let Some(agent) = self.cluster_epoch_ids(epoch_ids) {
                    return Poll::Ready(Some(HistorySyncReturn::Outdated(agent)));
                }
            }
        }

        Poll::Pending
    }

    fn poll_cluster(&mut self, cx: &mut Context<'_>) {
        // Initialize active_cluster if there is none.
        if self.active_cluster.is_none() {
            self.active_cluster = self.pop_next_cluster();
        }

        // Poll the active cluster.
        if let Some(cluster) = self.active_cluster.as_mut() {
            while self.job_queue.len() < Self::MAX_QUEUED_JOBS {
                let result = match cluster.poll_next_unpin(cx) {
                    Poll::Ready(result) => result,
                    Poll::Pending => break,
                };

                match result {
                    Some(Ok(batch_set)) => {
                        let hash = batch_set.block.hash();
                        let blockchain = Arc::clone(&self.blockchain);
                        let future = async move {
                            debug!(
                                "Processing epoch #{} ({} history items)",
                                batch_set.block.epoch_number(),
                                batch_set.history.len()
                            );
                            spawn_blocking(move || {
                                Blockchain::push_history_sync(
                                    blockchain.upgradable_read(),
                                    Block::Macro(batch_set.block),
                                    &batch_set.history,
                                )
                            })
                            .await
                            .expect("blockchain.push_history_sync() should not panic")
                            .into()
                        }
                        .boxed();

                        self.job_queue
                            .push_back(Job::PushBatchSet(cluster.id, hash, future));
                    }
                    Some(Err(_)) | None => {
                        // Cluster finished or errored, evict it.
                        let cluster = self.active_cluster.take().unwrap();

                        let result = match result {
                            Some(Err(e)) => e,
                            None => SyncClusterResult::NoMoreEpochs,
                            _ => unreachable!(),
                        };
                        self.job_queue
                            .push_back(Job::FinishCluster(cluster, result));

                        if let Some(waker) = self.waker.take() {
                            waker.wake();
                        }
                        break;
                    }
                }
            }
        }
    }

    fn poll_job_queue(&mut self, cx: &mut Context<'_>) {
        while let Some(job) = self.job_queue.front_mut() {
            let result = match job {
                Job::PushBatchSet(_, _, future) => match future.poll_unpin(cx) {
                    Poll::Ready(result) => Some(result),
                    Poll::Pending => break,
                },
                Job::FinishCluster(_, _) => None,
            };

            let job = self.job_queue.pop_front().unwrap();

            match job {
                Job::PushBatchSet(cluster_id, ..) => {
                    let result = result.unwrap();

                    log::debug!(
                        "PushBatchSet from cluster_id {} completed with result: {:?}",
                        cluster_id,
                        result
                    );

                    if result != SyncClusterResult::EpochSuccessful {
                        // The push operation failed, therefore the whole cluster is invalid.
                        // Clean out any jobs originating from the failed cluster from the job_queue.
                        // If the cluster isn't active anymore, get the cluster from the
                        // FinishCluster job in the job_queue.
                        let cluster = self.evict_jobs_by_cluster(cluster_id);

                        // If the failed cluster is the still active, we remove it.
                        let cluster = cluster.unwrap_or_else(|| {
                            self.active_cluster
                                .take()
                                .expect("No cluster in job_queue, active_cluster should exist")
                        });
                        assert_eq!(cluster_id, cluster.id);

                        self.finish_cluster(cluster, result);
                    }
                }
                Job::FinishCluster(cluster, result) => {
                    self.finish_cluster(cluster, result);
                }
            }

            if let Some(waker) = self.waker.take() {
                waker.wake();
            }
        }
    }

    fn evict_jobs_by_cluster(&mut self, cluster_id: usize) -> Option<SyncCluster<TNetwork>> {
        while let Some(job) = self.job_queue.front() {
            let id = match job {
                Job::PushBatchSet(cluster_id, ..) => *cluster_id,
                Job::FinishCluster(cluster, _) => cluster.id,
            };
            if id != cluster_id {
                return None;
            }
            let job = self.job_queue.pop_front().unwrap();
            if let Job::FinishCluster(cluster, _) = job {
                return Some(cluster);
            }
        }
        None
    }
}

impl<TNetwork: Network> Stream for HistorySync<TNetwork> {
    type Item = HistorySyncReturn<TNetwork::PeerType>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        store_waker!(self, waker, cx);

        if let Poll::Ready(o) = self.poll_network_events(cx) {
            return Poll::Ready(o);
        }

        if let Poll::Ready(o) = self.poll_epoch_ids(cx) {
            return Poll::Ready(o);
        }

        self.poll_cluster(cx);

        self.poll_job_queue(cx);

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::StreamExt;
    use nimiq_block_production::BlockProducer;
    use parking_lot::{RwLock, RwLockUpgradableReadGuard};

    use nimiq_blockchain::{AbstractBlockchain, Blockchain};
    use nimiq_database::volatile::VolatileEnvironment;
    use nimiq_network_interface::prelude::Network;
    use nimiq_network_mock::{MockHub, MockNetwork};
    use nimiq_primitives::networks::NetworkId;
    use nimiq_primitives::policy;
    use nimiq_test_utils::blockchain::{produce_macro_blocks_with_txns, signing_key, voting_key};
    use nimiq_utils::time::OffsetTime;

    use crate::messages::{RequestBatchSet, RequestBlockHashes, RequestHistoryChunk};
    use crate::sync::history::{HistorySync, HistorySyncReturn};
    use crate::Consensus;

    fn blockchain() -> Arc<RwLock<Blockchain>> {
        let time = Arc::new(OffsetTime::new());
        let env = VolatileEnvironment::new(10).unwrap();
        Arc::new(RwLock::new(
            Blockchain::new(env, NetworkId::UnitAlbatross, time).unwrap(),
        ))
    }

    fn copy_chain(from: &RwLock<Blockchain>, to: &RwLock<Blockchain>) {
        let chain_info =
            from.read()
                .chain_store
                .get_chain_info(&to.read().head_hash(), false, None);
        let mut block_hash = match chain_info {
            Some(chain_info) if chain_info.on_main_chain => chain_info.main_chain_successor,
            _ => panic!("Chains have diverged"),
        };

        while let Some(hash) = block_hash {
            let chain_info = from
                .read()
                .chain_store
                .get_chain_info(&hash, true, None)
                .unwrap();
            assert!(chain_info.on_main_chain);

            Blockchain::push(to.upgradable_read(), chain_info.head).expect("Failed to push block");
            block_hash = chain_info.main_chain_successor;
        }

        assert_eq!(from.read().head(), to.read().head());
    }

    fn spawn_request_handlers<TNetwork: Network>(
        network: &Arc<TNetwork>,
        blockchain: &Arc<RwLock<Blockchain>>,
    ) {
        tokio::spawn(Consensus::<TNetwork>::request_handler(
            network.receive_from_all::<RequestBlockHashes>(),
            blockchain,
        ));
        tokio::spawn(Consensus::<TNetwork>::request_handler(
            network.receive_from_all::<RequestBatchSet>(),
            blockchain,
        ));
        tokio::spawn(Consensus::<TNetwork>::request_handler(
            network.receive_from_all::<RequestHistoryChunk>(),
            blockchain,
        ));
    }

    #[tokio::test]
    async fn it_terminates_if_there_is_nothing_to_sync() {
        simple_logger::SimpleLogger::new()
            .with_level(actual_log::LevelFilter::Trace)
            .init()
            .ok();

        let mut hub = MockHub::default();
        let net1 = Arc::new(hub.new_network());
        let net2 = Arc::new(hub.new_network());

        let chain = blockchain();
        let mut sync = HistorySync::<MockNetwork>::new(Arc::clone(&chain), net1.subscribe_events());

        net1.dial_mock(&net2);
        spawn_request_handlers(&net2, &chain);

        match sync.next().await {
            Some(HistorySyncReturn::Good(_)) => {
                assert_eq!(chain.read().block_number(), 0);
            }
            _ => assert!(false, "Unexpected HistorySyncReturn"),
        }
    }

    #[tokio::test]
    async fn it_can_sync_a_single_finalized_epoch() {
        simple_logger::SimpleLogger::new()
            .with_level(actual_log::LevelFilter::Trace)
            .init()
            .ok();

        let mut hub = MockHub::default();
        let net1 = Arc::new(hub.new_network());
        let net2 = Arc::new(hub.new_network());

        let chain1 = blockchain();
        let chain2 = blockchain();

        let producer = BlockProducer::new(signing_key(), voting_key());
        produce_macro_blocks_with_txns(
            &producer,
            &chain2,
            policy::BATCHES_PER_EPOCH as usize,
            1,
            0,
        );
        assert_eq!(chain2.read().block_number(), policy::EPOCH_LENGTH);

        let mut sync =
            HistorySync::<MockNetwork>::new(Arc::clone(&chain1), net1.subscribe_events());

        net1.dial_mock(&net2);
        spawn_request_handlers(&net2, &chain2);

        match sync.next().await {
            Some(HistorySyncReturn::Good(_)) => {
                assert_eq!(chain1.read().head(), chain2.read().head());
            }
            _ => panic!("Unexpected HistorySyncReturn"),
        }
    }

    #[tokio::test]
    async fn it_can_sync_multiple_finalized_epochs() {
        simple_logger::SimpleLogger::new()
            .with_level(actual_log::LevelFilter::Trace)
            .init()
            .ok();

        let mut hub = MockHub::default();
        let net1 = Arc::new(hub.new_network());
        let net2 = Arc::new(hub.new_network());

        let chain1 = blockchain();
        let chain2 = blockchain();

        let num_epochs = 2;
        let producer = BlockProducer::new(signing_key(), voting_key());
        produce_macro_blocks_with_txns(
            &producer,
            &chain2,
            num_epochs * policy::BATCHES_PER_EPOCH as usize,
            1,
            0,
        );
        assert_eq!(
            chain2.read().block_number(),
            num_epochs as u32 * policy::EPOCH_LENGTH
        );

        let mut sync =
            HistorySync::<MockNetwork>::new(Arc::clone(&chain1), net1.subscribe_events());

        net1.dial_mock(&net2);
        spawn_request_handlers(&net2, &chain2);

        match sync.next().await {
            Some(HistorySyncReturn::Good(_)) => {
                assert_eq!(chain1.read().head(), chain2.read().head());
            }
            _ => panic!("Unexpected HistorySyncReturn"),
        }
    }

    #[tokio::test]
    async fn it_can_sync_a_single_batch() {
        simple_logger::SimpleLogger::new()
            .with_level(actual_log::LevelFilter::Trace)
            .init()
            .ok();

        let mut hub = MockHub::default();
        let net1 = Arc::new(hub.new_network());
        let net2 = Arc::new(hub.new_network());

        let chain1 = blockchain();
        let chain2 = blockchain();

        let producer = BlockProducer::new(signing_key(), voting_key());
        produce_macro_blocks_with_txns(&producer, &chain2, 1, 1, 0);
        assert_eq!(chain2.read().block_number(), policy::BATCH_LENGTH);

        let mut sync =
            HistorySync::<MockNetwork>::new(Arc::clone(&chain1), net1.subscribe_events());

        net1.dial_mock(&net2);
        spawn_request_handlers(&net2, &chain2);

        match sync.next().await {
            Some(HistorySyncReturn::Good(_)) => {
                assert_eq!(chain1.read().head(), chain2.read().head());
            }
            _ => panic!("Unexpected HistorySyncReturn"),
        }
    }

    #[tokio::test]
    async fn it_can_sync_multiple_batches() {
        simple_logger::SimpleLogger::new()
            .with_level(actual_log::LevelFilter::Trace)
            .init()
            .ok();

        let mut hub = MockHub::default();
        let net1 = Arc::new(hub.new_network());
        let net2 = Arc::new(hub.new_network());

        let chain1 = blockchain();
        let chain2 = blockchain();

        let num_batches = (policy::BATCHES_PER_EPOCH - 1) as usize;
        let producer = BlockProducer::new(signing_key(), voting_key());
        produce_macro_blocks_with_txns(&producer, &chain2, num_batches, 1, 0);
        assert_eq!(
            chain2.read().block_number(),
            num_batches as u32 * policy::BATCH_LENGTH
        );

        let mut sync =
            HistorySync::<MockNetwork>::new(Arc::clone(&chain1), net1.subscribe_events());

        net1.dial_mock(&net2);
        spawn_request_handlers(&net2, &chain2);

        match sync.next().await {
            Some(HistorySyncReturn::Good(_)) => {
                assert_eq!(chain1.read().head(), chain2.read().head());
            }
            _ => panic!("Unexpected HistorySyncReturn"),
        }
    }

    #[tokio::test]
    async fn it_can_sync_consecutive_batches_from_different_peers() {
        simple_logger::SimpleLogger::new()
            .with_level(actual_log::LevelFilter::Trace)
            .init()
            .ok();

        let mut hub = MockHub::default();
        let net1 = Arc::new(hub.new_network());
        let net2 = Arc::new(hub.new_network());
        let net3 = Arc::new(hub.new_network());
        let net4 = Arc::new(hub.new_network());

        let chain1 = blockchain();
        let chain2 = blockchain();
        let chain3 = blockchain();
        let chain4 = blockchain();

        let producer = BlockProducer::new(signing_key(), voting_key());
        produce_macro_blocks_with_txns(&producer, &chain2, 1, 1, 0);
        assert_eq!(chain2.read().block_number(), policy::BATCH_LENGTH);

        copy_chain(&*chain2, &*chain3);
        produce_macro_blocks_with_txns(&producer, &chain3, 1, 1, 0);
        assert_eq!(chain3.read().block_number(), 2 * policy::BATCH_LENGTH);

        copy_chain(&*chain3, &*chain4);
        produce_macro_blocks_with_txns(&producer, &chain4, 1, 1, 0);
        assert_eq!(chain4.read().block_number(), 3 * policy::BATCH_LENGTH);

        let mut sync =
            HistorySync::<MockNetwork>::new(Arc::clone(&chain1), net1.subscribe_events());

        net1.dial_mock(&net2);
        net1.dial_mock(&net3);
        net1.dial_mock(&net4);

        spawn_request_handlers(&net2, &chain2);
        spawn_request_handlers(&net3, &chain3);
        spawn_request_handlers(&net4, &chain4);

        log::info!(
            "Event 1: {:?}",
            tokio::time::timeout(std::time::Duration::from_secs(5), sync.next()).await
        );
        log::info!(
            "Event 2: {:?}",
            tokio::time::timeout(std::time::Duration::from_secs(5), sync.next()).await
        );
        // log::info!("Event 2: {:?}", sync.next().await);
        // log::info!("Event 3: {:?}", sync.next().await);

        // match sync.next().await {
        //     Some(HistorySyncReturn::Good(_)) => {
        //         assert_eq!(chain1.read().head(), chain2.read().head());
        //     }
        //     _ => panic!("Unexpected HistorySyncReturn"),
        // }
    }
}
