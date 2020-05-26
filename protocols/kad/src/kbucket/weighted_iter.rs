/*
 * Copyright 2020 Fluence Labs Limited
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use crate::kbucket::{
    BucketIndex, ClosestBucketsIter, Distance, KBucketsTable, KeyBytes, Node, NodeStatus,
};

#[derive(Copy, Clone)]
struct Progress {
    need: usize,
    got: usize,
}

enum State {
    Nowhere,
    Weighted(Progress),
    Swamp {
        /// After taking one element from swamp, go back to weighted, keeping saved progress
        saved: Option<Progress>,
    },
    Empty,
}

struct WeightedIter<'a, TKey, TVal> {
    start: BucketIndex,
    weighted_buckets: ClosestBucketsIter,
    weighted_iter: Option<Box<dyn Iterator<Item = (&'a Node<TKey, TVal>, NodeStatus)>>>,
    swamp_buckets: ClosestBucketsIter,
    swamp_iter: Option<Box<dyn Iterator<Item = (&'a Node<TKey, TVal>, NodeStatus)>>>,
    table: &'a KBucketsTable<TKey, TVal>, // TODO: make table &mut and call apply_pending?
    state: State,
}

impl<'a, TKey, TVal> WeightedIter<'a, TKey, TVal> {
    pub fn new(
        table: &'a KBucketsTable<TKey, TVal>,
        distance: Distance,
    ) -> impl Iterator<Item = (&'a Node<TKey, TVal>, NodeStatus)> + 'a
    where
        TKey: Clone + AsRef<KeyBytes>,
        TVal: Clone,
    {
        let start = BucketIndex::new(&distance).unwrap_or(BucketIndex(0));
        WeightedIter {
            start: start.clone(),
            weighted_buckets: ClosestBucketsIter::new(distance),
            weighted_iter: None,
            swamp_buckets: ClosestBucketsIter::new(distance),
            swamp_iter: None,
            table,
            state: State::Start,
        }
    }

    pub fn num_weighted(&self, idx: BucketIndex) -> usize {
        let distance = (self.start.get() as isize - idx.get() as isize).abs();
        // 16/2^(distance+2)
        16 / 2usize.pow((distance + 2) as u32) // TODO: check overflows?
    }

    pub fn num_swamp(_idx: BucketIndex) -> usize {
        1
    }

    // Take iterator for next weighted bucket, save it to self.weighted_iter, and return
    pub fn next_weighted_bucket(&mut self) -> Option<(BucketIndex, &'_ impl Iterator)> {
        if let Some(idx) = self.weighted_buckets.next() {
            let bucket = &self.table.buckets[idx.get()];
            self.weighted_iter = Some(Box::new(bucket.weighted()));
            self.weighted_iter.as_deref().map(|iter| (idx, iter))
        } else {
            None
        }
    }

    // Take next weighted element, return None when there's no weighted elements
    pub fn next_weighted(&mut self) -> Option<(&'a Node<TKey, TVal>, NodeStatus)> {
        // Take current weighted_iter or the next one
        let mut iter = self
            .weighted_iter
            .as_deref_mut()
            .or(self.next_weighted_bucket().as_mut())?;

        iter.next()
    }

    pub fn next_swamp_bucket(&mut self) -> Option<&'_ impl Iterator> {
        if let Some(idx) = self.swamp_buckets.next() {
            let bucket = &self.table.buckets[idx.get()];
            self.weighted_iter = Some(Box::new(bucket.swamp()));
            self.weighted_iter.as_deref()
        } else {
            None
        }
    }

    pub fn next_swamp(&mut self) -> Option<(&'a Node<TKey, TVal>, NodeStatus)> {
        let mut iter = self
            .swamp_iter
            .as_deref_mut()
            .or(self.next_swamp_bucket().as_mut())?;

        iter.next()
    }
}

impl<'a, TKey, TVal> Iterator for WeightedIter<'a, TKey, TVal>
where
    TKey: Clone + AsRef<KeyBytes>,
    TVal: Clone,
{
    type Item = (&'a Node<TKey, TVal>, NodeStatus);

    fn next(&mut self) -> Option<Self::Item> {
        use State::*;

        loop {
            let (state, result) = match &self.state {
                // Not yet started, or just finished a bucket
                // Here we decide where to go next
                Nowhere => {
                    // There are some weighted buckets
                    if let Some((idx, mut iter)) = self.next_weighted_bucket() {
                        if let Some(elem) = iter.next() {
                            // Found weighted element
                            let state = Weighted(Progress {
                                got: 1,
                                need: self.num_weighted(idx),
                            });
                            (state, Some(elem))
                        } else {
                            // Weighted bucket was empty: go decide again
                            (Nowhere, None)
                        }
                    } else {
                        // No weighted buckets, go to swamp
                        (Swamp { saved: None }, None)
                    }
                }
                // Iterating through weighted, need more
                Weighted(&Progress { got, need }) if got < need => {
                    if let Some(elem) = self.next_weighted() {
                        // Found weighted element, go take more
                        let state = Weighted(Progress { got: got + 1, need });
                        (state, Some(elem))
                    } else {
                        // Current bucket is empty, we're nowhere: need to decide where to go next
                        (Nowhere, None)
                    }
                }
                // Got enough weighted, go to swamp (saving progress, to return back with it)
                Weighted(&progress) => (
                    Swamp {
                        saved: Some(progress),
                    },
                    None,
                ),
                // Take one element from swamp, and go to Weighted
                Swamp {
                    saved: Some(&saved),
                } => {
                    if let Some(elem) = self.next_swamp() {
                        // We always take just a single element from the swamp
                        // And then go back to weighted
                        (Weighted(saved), Some(elem))
                    } else if self.next_swamp_bucket().is_some() {
                        // Current bucket was empty, go try next one
                        (Swamp { saved: Some(saved) }, None)
                    } else {
                        // No more swamp buckets, go drain weighted
                        (Weighted(saved), None)
                    }
                }
                // Weighted buckets are empty
                &Swamp { saved } => {
                    if let Some(elem) = self.next_swamp() {
                        (Swamp { saved }, Some(elem))
                    } else if self.next_swamp_bucket().is_some() {
                        // Current bucket was empty, go try next one
                        (Swamp { saved }, None)
                    } else {
                        // Routing table is empty
                        (Empty, None)
                    }
                }
            };

            self.state = state;

            if result.is_some() {
                return result;
            }

            if let state = Empty {
                return None;
            }
        }
    }
}
