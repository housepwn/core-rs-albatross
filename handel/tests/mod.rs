#[macro_use]
extern crate beserial_derive;

use async_trait::async_trait;
use beserial::{Deserialize, Serialize};
use nimiq_bls::PublicKey;
use nimiq_collections::bitset::BitSet;
use nimiq_handel::aggregation::Aggregation;
use nimiq_handel::config::Config;
use nimiq_handel::contribution::{AggregatableContribution, ContributionError};
use nimiq_handel::evaluator;
use nimiq_handel::identity;
use nimiq_handel::partitioner::BinomialPartitioner;
use nimiq_handel::protocol;
use nimiq_handel::store::ReplaceStore;
use nimiq_handel::verifier;
use nimiq_network_mock::network;

use std::sync::Arc;
use std::{fmt::Formatter, time::Duration};

use parking_lot::RwLock;

use tokio;

/// Dump Aggregate adding numbers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contribution {
    value: u64,
    contributors: BitSet,
}

impl AggregatableContribution for Contribution {
    fn contributors(&self) -> BitSet {
        self.contributors.clone()
    }

    fn combine(&mut self, other_contribution: &Self) -> Result<(), ContributionError> {
        let overlap = &self.contributors & &other_contribution.contributors;
        // only contributions without any overlap can be combined.
        if overlap.is_empty() {
            // the combined value is the addition of the 2 individual values
            self.value += other_contribution.value;
            // the contributors of the resulting contribution are the combined sets of both individual contributions.
            self.contributors = &self.contributors | &other_contribution.contributors;
            Ok(())
        } else {
            Err(ContributionError::Overlapping(overlap))
        }
    }
}

// dumb Registry
pub struct Registry {}
impl identity::WeightRegistry for Registry {
    fn weight(&self, _id: usize) -> Option<usize> {
        Some(1)
    }
}
impl identity::IdentityRegistry for Registry {
    fn public_key(&self, _id: usize) -> Option<PublicKey> {
        None
    }
}

// dump Verifier who is happy with everything.
pub struct DumbVerifier {}

#[async_trait]
impl verifier::Verifier for DumbVerifier {
    type Contribution = Contribution;
    async fn verify(&self, _contribution: &Self::Contribution) -> verifier::VerificationResult {
        verifier::VerificationResult::Ok
    }
}

pub type Store = ReplaceStore<BinomialPartitioner, Contribution>;

pub type Evaluator = evaluator::WeightedVote<Store, Registry, BinomialPartitioner>;

// The test protocol combining the other types.
pub struct Protocol {
    verifier: Arc<DumbVerifier>,
    partitioner: Arc<BinomialPartitioner>,
    evaluator: Arc<Evaluator>,
    store: Arc<RwLock<ReplaceStore<BinomialPartitioner, Contribution>>>,
    registry: Arc<Registry>,
    node_id: usize,
}
impl Protocol {
    pub fn new(node_id: usize, num_ids: usize, threshold: usize) -> Self {
        let partitioner = Arc::new(BinomialPartitioner::new(node_id, num_ids));
        let registry = Arc::new(Registry {});
        let store = Arc::new(RwLock::new(
            ReplaceStore::<BinomialPartitioner, Contribution>::new(partitioner.clone()),
        ));

        let evaluator = Arc::new(evaluator::WeightedVote::new(
            store.clone(),
            Arc::clone(&registry),
            partitioner.clone(),
            threshold,
        ));

        Protocol {
            verifier: Arc::new(DumbVerifier {}),
            partitioner,
            evaluator,
            store,
            registry: Arc::new(Registry {}),
            node_id,
        }
    }
}

impl std::fmt::Debug for Protocol {
    fn fmt(&self, _f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        Ok(())
    }
}

impl protocol::Protocol for Protocol {
    type Contribution = Contribution;
    type Verifier = DumbVerifier;
    type Registry = Registry;
    type Partitioner = BinomialPartitioner;
    type Store = Store;
    type Evaluator = evaluator::WeightedVote<Self::Store, Self::Registry, Self::Partitioner>;

    fn verifier(&self) -> Arc<Self::Verifier> {
        self.verifier.clone()
    }
    fn registry(&self) -> Arc<Self::Registry> {
        self.registry.clone()
    }
    fn store(&self) -> Arc<RwLock<Self::Store>> {
        self.store.clone()
    }
    fn evaluator(&self) -> Arc<Self::Evaluator> {
        self.evaluator.clone()
    }
    fn partitioner(&self) -> Arc<Self::Partitioner> {
        self.partitioner.clone()
    }
    fn node_id(&self) -> usize {
        self.node_id
    }
}

#[tokio::test]
async fn it_can_aggregate() {
    let config = Config {
        update_count: 4,
        update_interval: Duration::from_millis(100),
        timeout: Duration::from_millis(500),
        grace_period: Duration::from_millis(50),
        peer_count: 1,
    };

    let contributor_num: usize = 8;

    let mut networks: Vec<Arc<network::MockNetwork>> = vec![];
    // Initialize `contributor_num networks and Handel Aggregations. Connect all the networks with each other.
    for id in 0..contributor_num {
        // Create a network with id = `id`
        let net = Arc::new(network::MockNetwork::new(id));
        // Create a protocol with `contributor_num + 1` peers set its id to `id`. Require `contributor_num` contributions
        // meaning all contributions need to be aggregated with the additional node initialized after this for loop.
        let protocol = Protocol::new(id, contributor_num + 1, contributor_num);
        // the sole contributor for soon to be created contribution is this node.
        let mut contributors = BitSet::new();
        contributors.insert(id);

        // create a contribution for this node with a value of `id + 1` (So no node has value 0 which doesn't show up in addition).
        let contribution = Contribution {
            value: id as u64 + 1u64,
            contributors,
        };
        // connect the network to all already existing networks.
        for network in &networks {
            net.connect(network);
        }
        // remember the network so that subsequently created networks can connect to it.
        networks.push(net.clone());
        // spawn a task for this Handel Aggregation and Network instance.
        tokio::spawn(Aggregation::start(
            1 as u8, // serves as the tag or identifier for this aggregation
            contribution,
            protocol,
            config.clone(),
            net,
        ));
    }
    // same as in the for loop, execpt we want to keep the handel sinatnce and not spawn it.
    let net = Arc::new(network::MockNetwork::new(contributor_num));
    let protocol = Protocol::new(contributor_num, contributor_num + 1, contributor_num + 1);
    let mut contributors = BitSet::new();
    contributors.insert(contributor_num);
    let contribution = Contribution {
        value: contributor_num as u64 + 1u64,
        contributors,
    };
    for network in &networks {
        net.connect(network);
    }
    networks.push(net.clone());

    // instead of spawning the aggregation atsk await its result here.
    let aggregate: Contribution =
        Aggregation::start(1 as u8, contribution, protocol, config.clone(), net).await;

    println!("{:?}", &aggregate);

    // All nodes need to contribute
    assert_eq!(
        aggregate.num_contributors(),
        contributor_num + 1,
        "Not all contributions are present"
    );
    // the final value needs to be the sum of all contributions: 9 + 8 + 7 + 6 + 5 + 4 + 3 + 2 + 1 = 45
    assert_eq!(aggregate.value, 45, "Wrong aggregation result");
}

#[tokio::test]
async fn it_can_aggregate_to_treshold() {
    let config = Config {
        update_count: 4,
        update_interval: Duration::from_millis(100),
        timeout: Duration::from_millis(500),
        grace_period: Duration::from_millis(50),
        peer_count: 1,
    };

    let contributor_num: usize = 8;

    let mut networks: Vec<Arc<network::MockNetwork>> = vec![];
    // Initialize `contributor_num networks and Handel Aggregations. Connect all the networks with each other.
    for id in 0..contributor_num {
        // Create a network with id = `id`
        let net = Arc::new(network::MockNetwork::new(id));
        // Create a protocol with `contributor_num + 1` peers set its id to `id`. Require `contributor_num` contributions
        // meaning all contributions need to be aggregated with the additional node initialized after this for loop.
        let protocol = Protocol::new(id, contributor_num + 1, contributor_num / 2);
        // the sole contributor for soon to be created contribution is this node.
        let mut contributors = BitSet::new();
        contributors.insert(id);

        // create a contribution for this node with a value of `id +1` (So no node has value 0 as that does't show up in addition).
        let contribution = Contribution {
            value: 1u64,
            contributors,
        };
        // connect the network to all already existing networks.
        for network in &networks {
            net.connect(network);
        }
        // remember the network so that subsequently created networks can connect to it.
        networks.push(net.clone());
        // spawn a task for this Handel Aggregation and Network instance.
        tokio::spawn(Aggregation::start(
            1 as u8, // serves as the tag or identifier for this aggregation
            contribution,
            protocol,
            config.clone(),
            net,
        ));
    }
    // same as in the for loop, execpt we want to keep the handel sinatnce and not spawn it.
    let net = Arc::new(network::MockNetwork::new(contributor_num));
    let protocol = Protocol::new(contributor_num, contributor_num + 1, contributor_num / 2);
    let mut contributors = BitSet::new();
    contributors.insert(contributor_num);
    let contribution = Contribution {
        value: 1u64,
        contributors,
    };
    for network in &networks {
        net.connect(network);
    }
    networks.push(net.clone());

    // instead of spawning the aggregation atsk await its result here.
    let aggregate: Contribution =
        Aggregation::start(1 as u8, contribution, protocol, config.clone(), net).await;

    println!("{:?}", &aggregate);

    // At least more than half of the nodes need to contribute
    assert!(
        aggregate.num_contributors() >= contributor_num / 2,
        "Not enough contributions are present"
    );
    // the final value needs to be at least the sum of all contributions
    assert!(
        aggregate.value >= (contributor_num / 2) as u64,
        "Wrong aggregation result"
    );
}
