// Copyright 2019 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

#![cfg(test)]

use super::*;

use crate::K_VALUE;
use crate::kbucket::Distance;
use crate::record::{Key, store::MemoryStore};
use futures::{
    prelude::*,
    executor::block_on,
    future::poll_fn,
};
use futures_timer::Delay;
use libp2p_core::{
    connection::{ConnectedPoint, ConnectionId},
    PeerId,
    Transport,
    transport::MemoryTransport,
    multiaddr::{Protocol, Multiaddr, multiaddr},
    upgrade,
    multihash::{Code, Multihash, MultihashDigest},
};
use libp2p_noise as noise;
use libp2p_swarm::Swarm;
use libp2p_yamux as yamux;
use quickcheck::*;
use rand::{Rng, random, thread_rng, rngs::StdRng, SeedableRng};
use std::{collections::{HashSet, HashMap}, time::Duration, num::NonZeroUsize, u64};
use trust_graph::InMemoryStorage;

type TestSwarm = Swarm<Kademlia<MemoryStore>>;

fn build_node() -> (Keypair, Multiaddr, TestSwarm) {
    build_node_with_config(Default::default())
}

fn build_node_with_config(cfg: KademliaConfig) -> (Keypair, Multiaddr, TestSwarm) {
    let ed25519_key = Keypair::generate_ed25519();
    let local_key = ed25519_key.clone();
    let local_public_key = local_key.public();
    let noise_keys = noise::Keypair::<noise::X25519>::new().into_authentic(&local_key).unwrap();
    let transport = MemoryTransport::default()
        .upgrade(upgrade::Version::V1)
        .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
        .multiplex(yamux::YamuxConfig::default())
        .boxed();

    let local_id = local_public_key.clone().into_peer_id();
    let trust = {
        let pk = fluence_identity::PublicKey::from(ed25519_key.public());
        let storage = InMemoryStorage::new_in_memory(vec![(pk, 1)]);
        TrustGraph::new(storage)
    };
    let store = MemoryStore::new(local_id.clone());
    let behaviour = Kademlia::with_config(ed25519_key.clone(), local_id.clone(), store, cfg.clone(), trust);

    let mut swarm = Swarm::new(transport, behaviour, local_id);

    let address: Multiaddr = Protocol::Memory(random::<u64>()).into();
    Swarm::listen_on(&mut swarm, address.clone()).unwrap();

    (ed25519_key, address, swarm)
}

/// Builds swarms, each listening on a port. Does *not* connect the nodes together.
fn build_nodes(num: usize) -> Vec<(Keypair, Multiaddr, TestSwarm)> {
    build_nodes_with_config(num, Default::default())
}

/// Builds swarms, each listening on a port. Does *not* connect the nodes together.
fn build_nodes_with_config(num: usize, cfg: KademliaConfig) -> Vec<(Keypair, Multiaddr, TestSwarm)> {
    (0..num).map(|_| build_node_with_config(cfg.clone())).collect()
}

fn build_connected_nodes(total: usize, step: usize) -> Vec<(Keypair, Multiaddr, TestSwarm)> {
    build_connected_nodes_with_config(total, step, Default::default())
}

fn build_connected_nodes_with_config(total: usize, step: usize, cfg: KademliaConfig)
    -> Vec<(Keypair, Multiaddr, TestSwarm)>
{
    let mut swarms = build_nodes_with_config(total, cfg);
    let swarm_ids: Vec<_> = swarms.iter()
        .map(|(kp, addr, swarm)| (kp.public(), addr.clone(), Swarm::local_peer_id(swarm).clone()))
        .collect();

    let mut i = 0;
    let swarms_total = swarm_ids.len();
    for (j, (pk, addr, peer_id)) in swarm_ids.into_iter().enumerate().skip(1) {
        if i < swarms_total {
            swarms[i].2.add_address(&peer_id, addr.clone(), pk);
        }
        if j % step == 0 {
            i += step;
        }
    }

    swarms
}

fn build_fully_connected_nodes_with_config(total: usize, cfg: KademliaConfig)
    -> Vec<(Keypair, Multiaddr, TestSwarm)>
{
    let mut swarms = build_nodes_with_config(total, cfg);
    let swarm_addr_and_peer_id: Vec<_> = swarms.iter()
        .map(|(kp, addr, swarm)| (kp.public().clone(), addr.clone(), Swarm::local_peer_id(swarm).clone()))
        .collect();

    for (_, _addr, swarm) in swarms.iter_mut() {
        for (public, addr, peer) in &swarm_addr_and_peer_id {
            swarm.add_address(&peer, addr.clone(), public.clone());
        }
    }

    swarms
}

fn random_multihash() -> Multihash {
    Multihash::wrap(Code::Sha2_256.into(), &thread_rng().gen::<[u8; 32]>()).unwrap()
}

#[derive(Clone, Debug)]
struct Seed([u8; 32]);

impl Arbitrary for Seed {
    fn arbitrary<G: Gen>(g: &mut G) -> Seed {
        Seed(g.gen())
    }
}

#[test]
fn bootstrap() {
    fn prop(seed: Seed) {
        let mut rng = StdRng::from_seed(seed.0);

        let num_total = rng.gen_range(2, 20);
        // When looking for the closest node to a key, Kademlia considers
        // K_VALUE nodes to query at initialization. If `num_group` is larger
        // than K_VALUE the remaining locally known nodes will not be
        // considered. Given that no other node is aware of them, they would be
        // lost entirely. To prevent the above restrict `num_group` to be equal
        // or smaller than K_VALUE.
        let num_group = rng.gen_range(1, (num_total % K_VALUE.get()) + 2);

        let cfg = KademliaConfig::default();
        if rng.gen() {
            // TODO: fixme
            // cfg.disjoint_query_paths(true);
        }

        let mut swarms = build_connected_nodes_with_config(
            num_total,
            num_group,
            cfg,
        ).into_iter()
            .map(|(_, _a, s)| s)
            .collect::<Vec<_>>();

        let swarm_ids: Vec<_> = swarms.iter()
            .map(Swarm::local_peer_id)
            .cloned()
            .collect();

        let qid = swarms[0].bootstrap().unwrap();

        // Expected known peers
        let expected_known = swarm_ids.iter().skip(1).cloned().collect::<HashSet<_>>();
        let mut first = true;

        // Run test
        block_on(
            poll_fn(move |ctx| {
                for (i, swarm) in swarms.iter_mut().enumerate() {
                    loop {
                        match swarm.poll_next_unpin(ctx) {
                            Poll::Ready(Some(KademliaEvent::QueryResult {
                                id, result: QueryResult::Bootstrap(Ok(ok)), ..
                            })) => {
                                assert_eq!(id, qid);
                                assert_eq!(i, 0);
                                if first {
                                    // Bootstrapping must start with a self-lookup.
                                    assert_eq!(ok.peer, swarm_ids[0]);
                                }
                                first = false;
                                if ok.num_remaining == 0 {
                                    let mut known = HashSet::new();
                                    for b in swarm.kbuckets.iter() {
                                        for e in b.iter() {
                                            known.insert(e.node.key.preimage().clone());
                                        }
                                    }
                                    assert_eq!(expected_known, known);
                                    return Poll::Ready(())
                                }
                            }
                            // Ignore any other event.
                            Poll::Ready(Some(_)) => (),
                            e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                            Poll::Pending => break,
                        }
                    }
                }
                Poll::Pending
            })
        )
    }

    QuickCheck::new().tests(10).quickcheck(prop as fn(_) -> _)
}

#[test]
fn query_iter() {
    fn distances<K>(key: &kbucket::Key<K>, peers: Vec<PeerId>) -> Vec<Distance> {
        peers.into_iter()
            .map(kbucket::Key::from)
            .map(|k| k.distance(key))
            .collect()
    }

    fn run(rng: &mut impl Rng) {
        let num_total = rng.gen_range(2, 20);
        let mut swarms = build_connected_nodes(num_total, 1).into_iter()
            .map(|(_, _a, s)| s)
            .collect::<Vec<_>>();
        let swarm_ids: Vec<_> = swarms.iter().map(Swarm::local_peer_id).cloned().collect();

        // Ask the first peer in the list to search a random peer. The search should
        // propagate forwards through the list of peers.
        let search_target = PeerId::random();
        let search_target_key = kbucket::Key::from(search_target);
        let qid = swarms[0].get_closest_peers(search_target);

        match swarms[0].query(&qid) {
            Some(q) => match q.info() {
                QueryInfo::GetClosestPeers { key } => {
                    assert_eq!(&key[..], search_target.to_bytes().as_slice())
                },
                i => panic!("Unexpected query info: {:?}", i)
            }
            None => panic!("Query not found: {:?}", qid)
        }

        // Set up expectations.
        let expected_swarm_id = swarm_ids[0].clone();
        let expected_peer_ids: Vec<_> = swarm_ids.iter().skip(1).cloned().collect();
        let mut expected_distances = distances(&search_target_key, expected_peer_ids.clone());
        expected_distances.sort();

        // Run test
        block_on(
            poll_fn(move |ctx| {
                for (i, swarm) in swarms.iter_mut().enumerate() {
                    loop {
                        match swarm.poll_next_unpin(ctx) {
                            Poll::Ready(Some(KademliaEvent::QueryResult {
                                id, result: QueryResult::GetClosestPeers(Ok(ok)), ..
                            })) => {
                                assert_eq!(id, qid);
                                assert_eq!(&ok.key[..], search_target.to_bytes().as_slice());
                                assert_eq!(swarm_ids[i], expected_swarm_id);
                                assert_eq!(swarm.queries.size(), 0);
                                assert!(expected_peer_ids.iter().all(|p| ok.peers.contains(p)));
                                let key = kbucket::Key::new(ok.key);
                                assert_eq!(expected_distances, distances(&key, ok.peers));
                                return Poll::Ready(());
                            }
                            // Ignore any other event.
                            Poll::Ready(Some(_)) => (),
                            e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                            Poll::Pending => break,
                        }
                    }
                }
                Poll::Pending
            })
        )
    }

    let mut rng = thread_rng();
    for _ in 0 .. 10 {
        run(&mut rng)
    }
}

#[test]
fn unresponsive_not_returned_direct() {
    // Build one node. It contains fake addresses to non-existing nodes. We ask it to find a
    // random peer. We make sure that no fake address is returned.

    let mut swarms = build_nodes(1).into_iter()
        .map(|(kp, _a, s)| (kp, s))
        .collect::<Vec<_>>();

    // Add fake addresses.
    for _ in 0 .. 10 {
        let public0 = swarms[0].0.public();
        swarms[0].1.add_address(&PeerId::random(), Protocol::Udp(10u16).into(), public0);
    }

    // Ask first to search a random value.
    let search_target = PeerId::random();
    swarms[0].1.get_closest_peers(search_target);

    block_on(
        poll_fn(move |ctx| {
            for (_, swarm) in &mut swarms {
                loop {
                    match swarm.poll_next_unpin(ctx) {
                        Poll::Ready(Some(KademliaEvent::QueryResult {
                            result: QueryResult::GetClosestPeers(Ok(ok)), ..
                        })) => {
                            assert_eq!(&ok.key[..], search_target.to_bytes().as_slice());
                            assert_eq!(ok.peers.len(), 0);
                            return Poll::Ready(());
                        }
                        // Ignore any other event.
                        Poll::Ready(Some(_)) => (),
                        e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                        Poll::Pending => break,
                    }
                }
            }

            Poll::Pending
        })
    )
}

#[test]
fn unresponsive_not_returned_indirect() {
    // Build two nodes. Node #2 knows about node #1. Node #1 contains fake addresses to
    // non-existing nodes. We ask node #2 to find a random peer. We make sure that no fake address
    // is returned.

    let mut swarms = build_nodes(2);

    // Add fake addresses to first.
    for _ in 0 .. 10 {
        let public0 = swarms[0].0.public();
        swarms[0].2.add_address(&PeerId::random(), multiaddr![Udp(10u16)], public0);
    }

    // Connect second to first.
    let first_peer_id = Swarm::local_peer_id(&swarms[0].2).clone();
    let first_address = swarms[0].1.clone();
    let public1 = swarms[1].0.public();
    swarms[1].2.add_address(&first_peer_id, first_address, public1);

    // Drop the swarm addresses.
    let mut swarms = swarms.into_iter().map(|(_, _addr, swarm)| swarm).collect::<Vec<_>>();

    // Ask second to search a random value.
    let search_target = PeerId::random();
    swarms[1].get_closest_peers(search_target);

    block_on(
        poll_fn(move |ctx| {
            for swarm in &mut swarms {
                loop {
                    match swarm.poll_next_unpin(ctx) {
                        Poll::Ready(Some(KademliaEvent::QueryResult {
                            result: QueryResult::GetClosestPeers(Ok(ok)), ..
                        })) => {
                            assert_eq!(&ok.key[..], search_target.to_bytes().as_slice());
                            assert_eq!(ok.peers.len(), 1);
                            assert_eq!(ok.peers[0], first_peer_id);
                            return Poll::Ready(());
                        }
                        // Ignore any other event.
                        Poll::Ready(Some(_)) => (),
                        e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                        Poll::Pending => break,
                    }
                }
            }

            Poll::Pending
        })
    )
}

#[test]
fn get_record_not_found() {
    let mut swarms = build_nodes(3);

    let swarm_ids: Vec<_> = swarms.iter()
        .map(|(_, _addr, swarm)| Swarm::local_peer_id(swarm))
        .cloned()
        .collect();

    let public1 = swarms[1].0.public();
    let public2 = swarms[1].0.public();
    let (second, third) = (swarms[1].1.clone(), swarms[2].1.clone());
    swarms[0].2.add_address(&swarm_ids[1], second, public1);
    swarms[1].2.add_address(&swarm_ids[2], third, public2);

    // Drop the swarm addresses.
    let mut swarms = swarms.into_iter().map(|(_, _addr, swarm)| swarm).collect::<Vec<_>>();

    let target_key = record::Key::from(random_multihash());
    let qid = swarms[0].get_record(&target_key, Quorum::One);

    block_on(
        poll_fn(move |ctx| {
            for swarm in &mut swarms {
                loop {
                    match swarm.poll_next_unpin(ctx) {
                        Poll::Ready(Some(KademliaEvent::QueryResult {
                            id, result: QueryResult::GetRecord(Err(e)), ..
                        })) => {
                            assert_eq!(id, qid);
                            if let GetRecordError::NotFound { key, closest_peers, } = e {
                                assert_eq!(key, target_key);
                                assert_eq!(closest_peers.len(), 2);
                                assert!(closest_peers.contains(&swarm_ids[1]));
                                assert!(closest_peers.contains(&swarm_ids[2]));
                                return Poll::Ready(());
                            } else {
                                panic!("Unexpected error result: {:?}", e);
                            }
                        }
                        // Ignore any other event.
                        Poll::Ready(Some(_)) => (),
                        e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                        Poll::Pending => break,
                    }
                }
            }

            Poll::Pending
        })
    )
}

/// A node joining a fully connected network via three (ALPHA_VALUE) bootnodes
/// should be able to put a record to the X closest nodes of the network where X
/// is equal to the configured replication factor.
#[test]
fn put_record() {
    fn prop(records: Vec<Record>, seed: Seed) {
        let mut rng = StdRng::from_seed(seed.0);
        let replication_factor = NonZeroUsize::new(rng.gen_range(1, (K_VALUE.get() / 2) + 1)).unwrap();
        // At least 4 nodes, 1 under test + 3 bootnodes.
        let num_total = usize::max(4, replication_factor.get() * 2);

        let mut config = KademliaConfig::default();
        config.set_replication_factor(replication_factor);
        if rng.gen() {
            // TODO FIXME: disjoint paths are not working correctly with weighted and swamp buckets
            // config.disjoint_query_paths(true);
        }

        let mut swarms = {
            let mut fully_connected_swarms = build_fully_connected_nodes_with_config(
                num_total - 1,
                config.clone(),
            );

            let mut single_swarm = build_node_with_config(config);
            // Connect `single_swarm` to three bootnodes.
            for i in 0..3 {
                single_swarm.2.add_address(
                    &Swarm::local_peer_id(&fully_connected_swarms[0].2),
                fully_connected_swarms[i].1.clone(),
                    fully_connected_swarms[i].0.public(),
                );
            }

            let mut swarms = vec![single_swarm];
            swarms.append(&mut fully_connected_swarms);

            // Drop the swarm addresses.
            swarms.into_iter().map(|(_, _addr, swarm)| swarm).collect::<Vec<_>>()
        };

        let records = records.into_iter()
            .take(num_total)
            .map(|mut r| {
                // We don't want records to expire prematurely, as they would
                // be removed from storage and no longer replicated, but we still
                // want to check that an explicitly set expiration is preserved.
                r.expires = r.expires.map(|t| t + Duration::from_secs(60));
                (r.key.clone(), r)
            })
            .collect::<HashMap<_,_>>();

        // Initiate put_record queries.
        let mut qids = HashSet::new();
        for r in records.values() {
            let qid = swarms[0].put_record(r.clone(), Quorum::All).unwrap();
            match swarms[0].query(&qid) {
                Some(q) => match q.info() {
                    QueryInfo::PutRecord { phase, record, .. } => {
                        assert_eq!(phase, &PutRecordPhase::GetClosestPeers);
                        assert_eq!(record.key, r.key);
                        assert_eq!(record.value, r.value);
                        assert!(record.expires.is_some());
                        qids.insert(qid);
                    },
                    i => panic!("Unexpected query info: {:?}", i)
                }
                None => panic!("Query not found: {:?}", qid)
            }
        }

        // Each test run republishes all records once.
        let mut republished = false;
        // The accumulated results for one round of publishing.
        let mut results = Vec::new();

        block_on(
            poll_fn(move |ctx| loop {
                // Poll all swarms until they are "Pending".
                for swarm in &mut swarms {
                    loop {
                        match swarm.poll_next_unpin(ctx) {
                            Poll::Ready(Some(KademliaEvent::QueryResult {
                                id, result: QueryResult::PutRecord(res), stats
                            })) |
                            Poll::Ready(Some(KademliaEvent::QueryResult {
                                id, result: QueryResult::RepublishRecord(res), stats
                            })) => {
                                assert!(qids.is_empty() || qids.remove(&id));
                                assert!(stats.duration().is_some());
                                assert!(stats.num_successes() >= replication_factor.get() as u32);
                                assert!(stats.num_requests() >= stats.num_successes());
                                assert_eq!(stats.num_failures(), 0);
                                match res {
                                    Err(e) => panic!("{:?}", e),
                                    Ok(ok) => {
                                        assert!(records.contains_key(&ok.key));
                                        let record = swarm.store.get(&ok.key).unwrap();
                                        results.push(record.into_owned());
                                    }
                                }
                            }
                            // Ignore any other event.
                            Poll::Ready(Some(_)) => (),
                            e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                            Poll::Pending => break,
                        }
                    }
                }

                // All swarms are Pending and not enough results have been collected
                // so far, thus wait to be polled again for further progress.
                if results.len() != records.len() {
                    return Poll::Pending
                }

                // Consume the results, checking that each record was replicated
                // correctly to the closest peers to the key.
                while let Some(r) = results.pop() {
                    let expected = records.get(&r.key).unwrap();

                    assert_eq!(r.key, expected.key);
                    assert_eq!(r.value, expected.value);
                    assert_eq!(r.expires, expected.expires);
                    assert_eq!(r.publisher.as_ref(), Some(Swarm::local_peer_id(&swarms[0])));

                    let key = kbucket::Key::new(r.key.clone());
                    let mut expected = swarms.iter()
                        .skip(1)
                        .map(Swarm::local_peer_id)
                        .cloned()
                        .collect::<Vec<_>>();
                    expected.sort_by(|id1, id2|
                        kbucket::Key::from(*id1).distance(&key).cmp(
                            &kbucket::Key::from(*id2).distance(&key)));

                    let expected = expected
                        .into_iter()
                        .take(replication_factor.get())
                        .collect::<HashSet<_>>();

                    let actual = swarms.iter()
                        .skip(1)
                        .filter_map(|swarm|
                            if swarm.store.get(key.preimage()).is_some() {
                                Some(Swarm::local_peer_id(swarm).clone())
                            } else {
                                None
                            })
                        .collect::<HashSet<_>>();

                    assert_eq!(actual.len(), replication_factor.get());

                    let actual_not_expected = actual.difference(&expected)
                        .collect::<Vec<&PeerId>>();
                    assert!(
                        actual_not_expected.is_empty(),
                        "Did not expect records to be stored on nodes {:?}.",
                        actual_not_expected,
                    );

                    let expected_not_actual = expected.difference(&actual)
                        .collect::<Vec<&PeerId>>();
                    assert!(expected_not_actual.is_empty(),
                           "Expected record to be stored on nodes {:?}.",
                           expected_not_actual,
                    );
                }

                if republished {
                    assert_eq!(swarms[0].store.records().count(), records.len());
                    assert_eq!(swarms[0].queries.size(), 0);
                    for k in records.keys() {
                        swarms[0].store.remove(&k);
                    }
                    assert_eq!(swarms[0].store.records().count(), 0);
                    // All records have been republished, thus the test is complete.
                    return Poll::Ready(());
                }

                // Tell the replication job to republish asap.
                swarms[0].put_record_job.as_mut().unwrap().asap(true);
                republished = true;
            })
        )
    }

    QuickCheck::new().tests(3).quickcheck(prop as fn(_,_) -> _)
}

#[test]
fn get_record() {
    let mut swarms = build_nodes(3);

    // Let first peer know of second peer and second peer know of third peer.
    for i in 0..2 {
        let (peer_id, public, address) = (Swarm::local_peer_id(&swarms[i+1].2).clone(), swarms[i+1].0.public(), swarms[i+1].1.clone());
        swarms[i].2.add_address(&peer_id, address, public);
    }

    // Drop the swarm addresses.
    let mut swarms = swarms.into_iter().map(|(kp, _addr, swarm)| (kp, swarm)).collect::<Vec<_>>();

    let record = Record::new(random_multihash(), vec![4,5,6]);

    swarms[1].1.store.put(record.clone()).unwrap();
    let qid = swarms[0].1.get_record(&record.key, Quorum::One);

    block_on(
        poll_fn(move |ctx| {
            for (_, swarm) in &mut swarms {
                loop {
                    match swarm.poll_next_unpin(ctx) {
                        Poll::Ready(Some(KademliaEvent::QueryResult {
                            id,
                            result: QueryResult::GetRecord(Ok(GetRecordOk { records })),
                            ..
                        })) => {
                            assert_eq!(id, qid);
                            assert_eq!(records.len(), 1);
                            assert_eq!(records.first().unwrap().record, record);
                            return Poll::Ready(());
                        }
                        // Ignore any other event.
                        Poll::Ready(Some(_)) => (),
                        e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                        Poll::Pending => break,
                    }
                }
            }

            Poll::Pending
        })
    )
}

#[test]
fn get_record_many() {
    // TODO: Randomise
    let num_nodes = 12;
    let mut swarms = build_connected_nodes(num_nodes, 3).into_iter()
        .map(|(_kp, _addr, swarm)| swarm)
        .collect::<Vec<_>>();
    let num_results = 10;

    let record = Record::new(random_multihash(), vec![4,5,6]);

    for i in 0 .. num_nodes {
        swarms[i].store.put(record.clone()).unwrap();
    }

    let quorum = Quorum::N(NonZeroUsize::new(num_results).unwrap());
    let qid = swarms[0].get_record(&record.key, quorum);

    block_on(
        poll_fn(move |ctx| {
            for swarm in &mut swarms {
                loop {
                    match swarm.poll_next_unpin(ctx) {
                        Poll::Ready(Some(KademliaEvent::QueryResult {
                            id,
                            result: QueryResult::GetRecord(Ok(GetRecordOk { records })),
                            ..
                        })) => {
                            assert_eq!(id, qid);
                            assert!(records.len() >= num_results);
                            assert!(records.into_iter().all(|r| r.record == record));
                            return Poll::Ready(());
                        }
                        // Ignore any other event.
                        Poll::Ready(Some(_)) => (),
                        e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                        Poll::Pending => break,
                    }
                }
            }
            Poll::Pending
        })
    )
}

/// A node joining a fully connected network via three (ALPHA_VALUE) bootnodes
/// should be able to add itself as a provider to the X closest nodes of the
/// network where X is equal to the configured replication factor.
#[test]
fn add_provider() {
    fn prop(keys: Vec<record::Key>, seed: Seed) {
        let mut rng = StdRng::from_seed(seed.0);
        let replication_factor = NonZeroUsize::new(rng.gen_range(1, (K_VALUE.get() / 2) + 1)).unwrap();
        // At least 4 nodes, 1 under test + 3 bootnodes.
        let num_total = usize::max(4, replication_factor.get() * 2);

        let mut config = KademliaConfig::default();
        config.set_replication_factor(replication_factor);
        if rng.gen() {
            // TODO: fixme
            // config.disjoint_query_paths(true);
        }

        let mut swarms = {
            let mut fully_connected_swarms = build_fully_connected_nodes_with_config(
                num_total - 1,
                config.clone(),
            );

            let mut single_swarm = build_node_with_config(config);
            // Connect `single_swarm` to three bootnodes.
            for i in 0..3 {
                single_swarm.2.add_address(
                    &Swarm::local_peer_id(&fully_connected_swarms[0].2),
                fully_connected_swarms[i].1.clone(),
                    fully_connected_swarms[i].0.public(),
                );
            }

            let mut swarms = vec![single_swarm];
            swarms.append(&mut fully_connected_swarms);

            // Drop addresses before returning.
            swarms.into_iter().map(|(_, _addr, swarm)| swarm).collect::<Vec<_>>()
        };

        let keys: HashSet<_> = keys.into_iter().take(num_total).collect();

        // Each test run publishes all records twice.
        let mut published = false;
        let mut republished = false;
        // The accumulated results for one round of publishing.
        let mut results = Vec::new();

        // Initiate the first round of publishing.
        let mut qids = HashSet::new();
        for k in &keys {
            let qid = swarms[0].start_providing(k.clone()).unwrap();
            qids.insert(qid);
        }

        block_on(
            poll_fn(move |ctx| loop {
                // Poll all swarms until they are "Pending".
                for swarm in &mut swarms {
                    loop {
                        match swarm.poll_next_unpin(ctx) {
                            Poll::Ready(Some(KademliaEvent::QueryResult {
                                id, result: QueryResult::StartProviding(res), ..
                            })) |
                            Poll::Ready(Some(KademliaEvent::QueryResult {
                                id, result: QueryResult::RepublishProvider(res), ..
                            })) => {
                                assert!(qids.is_empty() || qids.remove(&id));
                                match res {
                                    Err(e) => panic!(e),
                                    Ok(ok) => {
                                        assert!(keys.contains(&ok.key));
                                        results.push(ok.key);
                                    }
                                }
                            }
                            // Ignore any other event.
                            Poll::Ready(Some(_)) => (),
                            e @ Poll::Ready(_) => panic!("Unexpected return value: {:?}", e),
                            Poll::Pending => break,
                        }
                    }
                }

                if results.len() == keys.len() {
                    // All requests have been sent for one round of publishing.
                    published = true
                }

                if !published {
                    // Still waiting for all requests to be sent for one round
                    // of publishing.
                    return Poll::Pending
                }

                // A round of publishing is complete. Consume the results, checking that
                // each key was published to the `replication_factor` closest peers.
                while let Some(key) = results.pop() {
                    // Collect the nodes that have a provider record for `key`.
                    let actual = swarms.iter().skip(1)
                        .filter_map(|swarm|
                            if swarm.store.providers(&key).len() == 1 {
                                Some(Swarm::local_peer_id(&swarm).clone())
                            } else {
                                None
                            })
                        .collect::<HashSet<_>>();

                    if actual.len() != replication_factor.get() {
                        // Still waiting for some nodes to process the request.
                        results.push(key);
                        return Poll::Pending
                    }

                    let mut expected = swarms.iter()
                        .skip(1)
                        .map(Swarm::local_peer_id)
                        .cloned()
                        .collect::<Vec<_>>();
                    let kbucket_key = kbucket::Key::new(key);
                    expected.sort_by(|id1, id2|
                        kbucket::Key::from(*id1).distance(&kbucket_key).cmp(
                            &kbucket::Key::from(*id2).distance(&kbucket_key)));

                    let expected = expected
                        .into_iter()
                        .take(replication_factor.get())
                        .collect::<HashSet<_>>();

                    assert_eq!(actual, expected);
                }

                // One round of publishing is complete.
                assert!(results.is_empty());
                for swarm in &swarms {
                    assert_eq!(swarm.queries.size(), 0);
                }

                if republished {
                    assert_eq!(swarms[0].store.provided().count(), keys.len());
                    for k in &keys {
                        swarms[0].stop_providing(&k);
                    }
                    assert_eq!(swarms[0].store.provided().count(), 0);
                    // All records have been republished, thus the test is complete.
                    return Poll::Ready(());
                }

                // Initiate the second round of publishing by telling the
                // periodic provider job to run asap.
                swarms[0].add_provider_job.as_mut().unwrap().asap();
                published = false;
                republished = true;
            })
        )
    }

    QuickCheck::new().tests(3).quickcheck(prop as fn(_,_))
}

/// User code should be able to start queries beyond the internal
/// query limit for background jobs. Originally this even produced an
/// arithmetic overflow, see https://github.com/libp2p/rust-libp2p/issues/1290.
#[test]
fn exceed_jobs_max_queries() {
    let (_, _addr, mut swarm) = build_node();
    let num = JOBS_MAX_QUERIES + 1;
    for _ in 0 .. num {
        swarm.get_closest_peers(PeerId::random());
    }

    assert_eq!(swarm.queries.size(), num);

    block_on(
        poll_fn(move |ctx| {
            for _ in 0 .. num {
                // There are no other nodes, so the queries finish instantly.
                if let Poll::Ready(Some(e)) = swarm.poll_next_unpin(ctx) {
                    if let KademliaEvent::QueryResult {
                        result: QueryResult::GetClosestPeers(Ok(r)), ..
                    } = e {
                        assert!(r.peers.is_empty())
                    } else {
                        panic!("Unexpected event: {:?}", e)
                    }
                } else {
                    panic!("Expected event")
                }
            }
            Poll::Ready(())
        })
    )
}

#[test]
fn exp_decr_expiration_overflow() {
    fn prop_no_panic(ttl: Duration, factor: u32) {
        exp_decrease(ttl, factor);
    }

    // Right shifting a u64 by >63 results in a panic.
    prop_no_panic(KademliaConfig::default().record_ttl.unwrap(), 64);

    quickcheck(prop_no_panic as fn(_, _))
}

// TODO FIXME: disjoint paths are not working correctly with weighted and swamp
#[ignore]
#[test]
fn disjoint_query_does_not_finish_before_all_paths_did() {
    let mut config = KademliaConfig::default();
    config.disjoint_query_paths(true);
    // I.e. setting the amount disjoint paths to be explored to 2.
    config.set_parallelism(NonZeroUsize::new(2).unwrap());

    let mut alice = build_node_with_config(config);
    let mut trudy = build_node(); // Trudy the intrudor, an adversary.
    let mut bob = build_node();

    let key = Key::from(Code::Sha2_256.digest(&thread_rng().gen::<[u8; 32]>()));
    let record_bob = Record::new(key.clone(), b"bob".to_vec());
    let record_trudy = Record::new(key.clone(), b"trudy".to_vec());

    // Make `bob` and `trudy` aware of their version of the record searched by
    // `alice`.
    bob.2.store.put(record_bob.clone()).unwrap();
    trudy.2.store.put(record_trudy.clone()).unwrap();

    // Make `trudy` and `bob` known to `alice`.
    alice.2.add_address(&Swarm::local_peer_id(&trudy.2), trudy.1.clone(), trudy.0.public());
    alice.2.add_address(&Swarm::local_peer_id(&bob.2), bob.1.clone(), bob.0.public());

    // Drop the swarm addresses.
    let (mut alice, mut bob, mut trudy) = (alice.2, bob.2, trudy.2);

    // Have `alice` query the Dht for `key` with a quorum of 1.
    alice.get_record(&key, Quorum::One);

    // The default peer timeout is 10 seconds. Choosing 1 seconds here should
    // give enough head room to prevent connections to `bob` to time out.
    let mut before_timeout = Delay::new(Duration::from_secs(1));

    // Poll only `alice` and `trudy` expecting `alice` not yet to return a query
    // result as it is not able to connect to `bob` just yet.
    block_on(
        poll_fn(|ctx| {
            for (i, swarm) in [&mut alice, &mut trudy].iter_mut().enumerate() {
                loop {
                    match swarm.poll_next_unpin(ctx) {
                        Poll::Ready(Some(KademliaEvent::QueryResult{
                            result: QueryResult::GetRecord(result),
                             ..
                        })) => {
                            if i != 0 {
                                panic!("Expected `QueryResult` from Alice.")
                            }

                            match result {
                                Ok(_) => panic!(
                                    "Expected query not to finish until all \
                                     disjoint paths have been explored.",
                                ),
                                Err(e) => panic!("{:?}", e),
                            }
                        }
                        // Ignore any other event.
                        Poll::Ready(Some(_)) => (),
                        Poll::Ready(None) => panic!("Expected Kademlia behaviour not to finish."),
                        Poll::Pending => break,
                    }
                }
            }

            // Make sure not to wait until connections to `bob` time out.
            before_timeout.poll_unpin(ctx)
        })
    );

    // Make sure `alice` has exactly one query with `trudy`'s record only.
    assert_eq!(1, alice.queries.iter().count());
    alice.queries.iter().for_each(|q| {
        match &q.inner.info {
            QueryInfo::GetRecord{ records, .. }  => {
                assert_eq!(
                    *records,
                    vec![PeerRecord {
                        peer: Some(Swarm::local_peer_id(&trudy).clone()),
                        record: record_trudy.clone(),
                    }],
                );
            },
            i @ _ => panic!("Unexpected query info: {:?}", i),
        }
    });

    // Poll `alice` and `bob` expecting `alice` to return a successful query
    // result as it is now able to explore the second disjoint path.
    let records = block_on(
        poll_fn(|ctx| {
            for (i, swarm) in [&mut alice, &mut bob].iter_mut().enumerate() {
                loop {
                    match swarm.poll_next_unpin(ctx) {
                        Poll::Ready(Some(KademliaEvent::QueryResult{
                            result: QueryResult::GetRecord(result),
                            ..
                        })) => {
                            if i != 0 {
                                panic!("Expected `QueryResult` from Alice.")
                            }

                            match result {
                                Ok(ok) => return Poll::Ready(ok.records),
                                Err(e) => unreachable!("{:?}", e),
                            }
                        }
                        // Ignore any other event.
                        Poll::Ready(Some(_)) => (),
                        Poll::Ready(None) => panic!(
                            "Expected Kademlia behaviour not to finish.",
                        ),
                        Poll::Pending => break,
                    }
                }
            }

            Poll::Pending
        })
    );

    assert_eq!(2, records.len());
    assert!(records.contains(&PeerRecord {
        peer: Some(Swarm::local_peer_id(&bob).clone()),
        record: record_bob,
    }));
    assert!(records.contains(&PeerRecord {
        peer: Some(Swarm::local_peer_id(&trudy).clone()),
        record: record_trudy,
    }));
}

/// Tests that peers are not automatically inserted into
/// the routing table with `KademliaBucketInserts::Manual`.
#[test]
fn manual_bucket_inserts() {
    let mut cfg = KademliaConfig::default();
    cfg.set_kbucket_inserts(KademliaBucketInserts::Manual);
    // 1 -> 2 -> [3 -> ...]
    let mut swarms = build_connected_nodes_with_config(3, 1, cfg);
    // The peers and their addresses for which we expect `RoutablePeer` events.
    let mut expected = swarms.iter().skip(2)
        .map(|(_, a, s)| (a.clone(), Swarm::local_peer_id(s).clone()))
        .collect::<HashMap<_,_>>();
    // We collect the peers for which a `RoutablePeer` event
    // was received in here to check at the end of the test
    // that none of them was inserted into a bucket.
    let mut routable = Vec::new();
    // Start an iterative query from the first peer.
    swarms[0].2.get_closest_peers(PeerId::random());
    block_on(poll_fn(move |ctx| {
        for (_, _, swarm) in swarms.iter_mut() {
            loop {
                match swarm.poll_next_unpin(ctx) {
                    Poll::Ready(Some(KademliaEvent::RoutablePeer {
                        peer, address
                    })) => {
                        assert_eq!(peer, expected.remove(&address).expect("Unexpected address"));
                        routable.push(peer);
                        if expected.is_empty() {
                            for peer in routable.iter() {
                                let bucket = swarm.kbucket(*peer).unwrap();
                                assert!(bucket.iter().all(|e| e.node.key.preimage() != peer));
                            }
                            return Poll::Ready(())
                        }
                    }
                    Poll::Ready(..) => {},
                    Poll::Pending => break
                }
            }
        }
        Poll::Pending
    }));
}

#[test]
fn network_behaviour_inject_address_change() {
    let (_, _, mut kademlia) = build_node();

    let remote_peer_id = PeerId::random_with_pk();
    let connection_id = ConnectionId::new(1);
    let old_address: Multiaddr = Protocol::Memory(1).into();
    let new_address: Multiaddr = Protocol::Memory(2).into();

    let endpoint = ConnectedPoint::Dialer { address:  old_address.clone() };

    // Mimick a connection being established.
    kademlia.inject_connection_established(
        &remote_peer_id,
        &connection_id,
        &endpoint,
    );
    kademlia.inject_connected(&remote_peer_id);

    // At this point the remote is not yet known to support the
    // configured protocol name, so the peer is not yet in the
    // local routing table and hence no addresses are known.
    assert!(kademlia.addresses_of_peer(&remote_peer_id).is_empty());

    // Mimick the connection handler confirming the protocol for
    // the test connection, so that the peer is added to the routing table.
    kademlia.inject_event(
        remote_peer_id.clone(),
        connection_id.clone(),
        KademliaHandlerEvent::ProtocolConfirmed { endpoint }
    );

    assert_eq!(
        vec![old_address.clone()],
        kademlia.addresses_of_peer(&remote_peer_id),
    );

    kademlia.inject_address_change(
        &remote_peer_id,
        &connection_id,
        &ConnectedPoint::Dialer { address: old_address.clone() },
        &ConnectedPoint::Dialer {  address: new_address.clone() },
    );

    assert_eq!(
        vec![new_address.clone()],
        kademlia.addresses_of_peer(&remote_peer_id),
    );
}

fn make_swarms(total: usize, config: KademliaConfig) -> Vec<(Keypair, Multiaddr, TestSwarm)> {
    let mut fully_connected_swarms = build_fully_connected_nodes_with_config(
        total - 1,
        config.clone(),
    );

    let mut single_swarm = build_node_with_config(config);
    // Connect `single_swarm` to three bootnodes.
    for i in 0..3 {
        single_swarm.2.add_address(
            Swarm::local_peer_id(&fully_connected_swarms[0].2),
            fully_connected_swarms[i].1.clone(),
            fully_connected_swarms[i].0.public(),
        );
    }

    let mut swarms = vec![single_swarm];
    swarms.append(&mut fully_connected_swarms);

    // Drop addresses before returning.
    swarms.into_iter().collect::<Vec<_>>()
}

#[cfg(test)]
mod certificates {
    use super::*;
    use trust_graph::current_time;
    use fluence_identity::{KeyPair, PublicKey};

    fn gen_root_cert(from: &KeyPair, to: PublicKey) -> Certificate {
        let cur_time = current_time();

        Certificate::issue_root(
            from,
            to,
            cur_time.checked_add(Duration::new(60, 0)).unwrap(),
            cur_time,
        )
    }

    fn gen_cert(from: &KeyPair, to: PublicKey, root: &Certificate) -> Certificate {
        let cur_time = current_time();

        Certificate::issue(
            from,
            to,
            root,
            cur_time.checked_add(Duration::new(60, 0)).unwrap(),
            cur_time,
            cur_time
        ).expect("issue cert")
    }

    struct SwarmWithKeypair {
        pub swarm: TestSwarm,
        pub kp: Keypair,
    }

    #[test]
    pub fn certificate_dissemination() {
        for _ in 1..10 {
            let total = 5; // minimum: 4
            let mut config = KademliaConfig::default();
            config.set_replication_factor(NonZeroUsize::new(total).unwrap());

            // each node has herself in the trust.root_weights
            let mut swarms = build_fully_connected_nodes_with_config(total, config);

            // Set same weights to all nodes, so they store each other's certificates
            let weights = swarms.iter().map(|(kp, _, _)| (kp.public(), 1)).collect::<Vec<_>>();
            for swarm in swarms.iter_mut() {
                for (pk, weight) in weights.iter() {
                    let pk = fluence_identity::PublicKey::from(pk.clone());
                    swarm.2.trust.add_root_weight(pk, *weight).expect("trust.add_root_weight failed");
                }
            }

            let mut swarms = swarms.into_iter();
            let (first_kp, _, first) = swarms.next().unwrap();
            // issue certs from each swarm to the first swarm, so all swarms trust the first one
            let mut swarms = swarms.map(|(kp, _, mut swarm)| {
                let pk = fluence_identity::PublicKey::from(first_kp.public());
                // root cert, its chain is [self-signed: swarm -> swarm, swarm -> first]
                let root = gen_root_cert(&kp.clone().into(), pk);
                swarm.trust.add(&root, current_time()).unwrap();
                SwarmWithKeypair { swarm, kp }
            });

            let mut swarm0 = SwarmWithKeypair { swarm: first, kp: first_kp.clone() };
            let swarm1 = swarms.next().unwrap();
            let mut swarm2 = swarms.next().unwrap();

            // issue cert from the first swarm to the second (will be later disseminated via kademlia)
            // chain: 0 -> 1
            let cert_0_1 = {
                let pk = fluence_identity::PublicKey::from(swarm1.kp.public());
                gen_root_cert(&swarm0.kp.clone().into(), pk)
            };
            swarm0.swarm.trust.add(&cert_0_1, current_time()).unwrap();
            let cert_0_1_check = {
                let pk = fluence_identity::PublicKey::from(swarm1.kp.public());
                swarm0.swarm.trust.get_all_certs(pk, &[]).unwrap()
            };
            assert_eq!(cert_0_1_check.len(), 1);
            let cert_0_1_check = cert_0_1_check.into_iter().nth(0).unwrap();
            assert_eq!(cert_0_1, cert_0_1_check);

            // check that this certificate (with root prepended) can be added to trust graph of any other node
            // chain: (2 -> 0)
            let mut cert_2_0_1 = {
                let pk = fluence_identity::PublicKey::from(swarm0.kp.public());
                gen_root_cert(&swarm2.kp.clone().into(), pk)
            };
            // chain: (2 -> 0) ++ (0 -> 1)
            cert_2_0_1.chain.extend_from_slice(&cert_0_1.chain[1..]);
            swarm2.swarm.trust.add(cert_2_0_1, current_time()).unwrap();

            // query kademlia
            let mut swarms = vec![swarm0, swarm1, swarm2].into_iter().chain(swarms).collect::<Vec<_>>();
            for swarm in swarms.iter_mut() {
                swarm.swarm.get_closest_peers(PeerId::random());
            }

            // wait for this number of queries to complete
            let mut queries = swarms.len();

            block_on(async move {
                // poll each swarm so they make a progress, wait for all queries to complete
                poll_fn(|ctx| {
                    for swarm in swarms.iter_mut() {
                        loop {
                            match swarm.swarm.poll_next_unpin(ctx) {
                                Poll::Ready(Some(KademliaEvent::QueryResult { .. })) => {
                                    queries -= 1;
                                    if queries == 0 {
                                        return Poll::Ready(())
                                    }
                                }
                                Poll::Ready(_) => {},
                                Poll::Pending => break,
                            }
                        }
                    }
                    Poll::Pending
                }).await;

                let kp_1 = swarms[1].kp.public();

                // check that certificates for `swarm[1].kp` were disseminated
                for swarm in swarms.iter().skip(2) {
                    let disseminated = {
                        let pk = fluence_identity::PublicKey::from(kp_1.clone());
                        swarm.swarm.trust.get_all_certs(&pk, &[]).unwrap()
                    };
                    // take only certificate converging to current `swarm` public key
                    let disseminated = {
                        let pk = fluence_identity::PublicKey::from(swarm.kp.public());
                        disseminated.into_iter().find(|c| &c.chain[0].issued_for == &pk).unwrap()
                    };
                    // swarm -> swarm0 -> swarm1
                    assert_eq!(disseminated.chain.len(), 3);
                    let pubkeys = disseminated.chain.iter().map(|c| &c.issued_for).collect::<Vec<_>>();
                    assert_eq!(
                        pubkeys,
                        vec![
                            &fluence_identity::PublicKey::from(swarm.kp.public()),
                            &fluence_identity::PublicKey::from(swarms[0].kp.public()),
                            &fluence_identity::PublicKey::from(swarms[1].kp.public()),
                        ]
                    );

                    // last trust in the certificate must be equal to previously generated (0 -> 1) trust
                    let last = disseminated.chain.last().unwrap();
                    assert_eq!(last, cert_0_1.chain.last().unwrap());
                }
            });
        }
    }
}