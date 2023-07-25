use std::{ops, sync::Arc};

use futures::future::BoxFuture;
use nimiq_network_interface::network::{Network, PubsubId};
use nimiq_primitives::{key_nibbles::KeyNibbles, trie::trie_diff::TrieDiff, TreeProof};
use parking_lot::RwLock;
use tokio::sync::Semaphore;

use super::{RequestPartialDiff, ResponsePartialDiff};
use crate::sync::{
    live::block_queue::BlockAndId,
    peer_list::{PeerList, PeerListIndex},
};

pub struct DiffRequestComponent<N: Network> {
    network: Arc<N>,
    peers: Arc<RwLock<PeerList<N>>>,
    current_peer_index: PeerListIndex,
    concurrent_requests: Arc<Semaphore>,
}

impl<N: Network> DiffRequestComponent<N> {
    const NUM_PENDING_DIFFS: usize = 5;

    pub fn new(network: Arc<N>, peers: Arc<RwLock<PeerList<N>>>) -> Self {
        DiffRequestComponent {
            network,
            peers,
            current_peer_index: PeerListIndex::default(),
            concurrent_requests: Arc::new(Semaphore::new(Self::NUM_PENDING_DIFFS)),
        }
    }

    pub fn request_diff(
        &mut self,
        range: ops::RangeTo<KeyNibbles>,
    ) -> impl FnMut(&BlockAndId<N>) -> BoxFuture<'static, Result<TrieDiff, ()>> {
        let mut starting_peer_index = self.current_peer_index.clone();
        self.current_peer_index.increment();

        let peers = Arc::clone(&self.peers);
        let network = Arc::clone(&self.network);
        let concurrent_requests = Arc::clone(&self.concurrent_requests);

        move |(block, pubsub_id)| {
            let peers = Arc::clone(&peers);

            // If we know the peer that sent us this block, we ask them first.
            let mut current_peer_index = pubsub_id
                .as_ref()
                .map(|id| id.propagation_source())
                .and_then(|peer_id| peers.read().index_of(&peer_id))
                .unwrap_or_else(|| {
                    starting_peer_index.increment();
                    starting_peer_index.clone()
                });

            let network = Arc::clone(&network);
            let concurrent_requests = Arc::clone(&concurrent_requests);
            let range = range.clone();
            let block_desc = format!("{}", block);
            let block_hash = block.hash();
            let block_diff_root = block.diff_root().clone();

            Box::pin(async move {
                let _request_permit = concurrent_requests.acquire().await.unwrap();
                let mut num_tries = 0;
                loop {
                    let peer_id = match peers.read().get(&current_peer_index) {
                        Some(peer_id) => peer_id,
                        None => {
                            error!("couldn't fetch diff: no peers");
                            return Err(());
                        }
                    };
                    current_peer_index.increment();

                    let result = network
                        .request(
                            RequestPartialDiff {
                                block_hash: block_hash.clone(),
                                range: range.clone(),
                            },
                            peer_id,
                        )
                        .await;

                    num_tries += 1;
                    let max_tries = peers.read().len();
                    let exhausted = num_tries >= max_tries;

                    match result {
                        Ok(ResponsePartialDiff::PartialDiff(diff)) => {
                            if TreeProof::new(diff.0.iter()).root_hash() == block_diff_root {
                                return Ok(diff);
                            }
                            error!(%peer_id, block = %block_desc, %num_tries, %max_tries, "couldn't fetch diff: invalid diff");
                        }
                        // TODO: remove peer, retry elsewhere
                        Ok(ResponsePartialDiff::IncompleteState) => {
                            if exhausted {
                                error!(%peer_id, block = %block_desc, %num_tries, %max_tries, "couldn't fetch diff: incomplete state")
                            } else {
                                debug!(%peer_id, block = %block_desc, %num_tries, %max_tries, "couldn't fetch diff: incomplete state")
                            }
                        }
                        Ok(ResponsePartialDiff::UnknownBlockHash) => {
                            if exhausted {
                                error!(%peer_id, block = %block_desc, %num_tries, %max_tries, "couldn't fetch diff: unknown block hash")
                            } else {
                                debug!(%peer_id, block = %block_desc, %num_tries, %max_tries, "couldn't fetch diff: unknown block hash")
                            }
                        }
                        Err(error) => {
                            error!(%peer_id, block = %block_desc, %num_tries, %max_tries, ?error, "couldn't fetch diff: {}", error)
                        }
                    }

                    if exhausted {
                        error!(%num_tries, %max_tries, "couldn't fetch diff: maximum tries reached");
                        return Err(());
                    }
                }
            })
        }
    }

    pub fn peer_list(&self) -> Arc<RwLock<PeerList<N>>> {
        Arc::clone(&self.peers)
    }
}
