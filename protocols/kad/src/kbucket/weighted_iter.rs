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

struct WeightedIter<'a, TKey, TVal> {
    start: BucketIndex,
    weighted_buckets: ClosestBucketsIter,
    weighted_iter: Option<Box<dyn Iterator<Item = (&'a Node<TKey, TVal>, NodeStatus)>>>,
    swamp_buckets: ClosestBucketsIter,
    swamp_iter: Option<Box<dyn Iterator<Item = (&'a Node<TKey, TVal>, NodeStatus)>>>,
    table: &'a KBucketsTable<TKey, TVal>, // TODO: make table &mut and call apply_pending?
    weighted_need: Option<usize>,
    weighted_got: Option<usize>,
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
        WeightedIter {
            weighted_buckets: ClosestBucketsIter::new(distance),
            weighted_iter: None,
            swamp_buckets: ClosestBucketsIter::new(distance),
            swamp_iter: None,
            start: BucketIndex::new(&distance).unwrap_or(BucketIndex(0)),
            table,
            weighted_need: None,
            weighted_got: None,
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
}

impl<'a, TKey, TVal> Iterator for WeightedIter<'a, TKey, TVal>
where
    TKey: Clone + AsRef<KeyBytes>,
    TVal: Clone,
{
    type Item = (&'a Node<TKey, TVal>, NodeStatus);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.weighted_got {
                // First iteration
                None => {
                    // Search through all weighted buckets, return first found element
                    // If no weighted elements were found, then search through all swamp buckets,
                    // return first found element
                    // If there's no elements, return None.
                    if let Some(idx) = self.weighted_buckets.next() {
                        let bucket = &self.table.buckets[idx.get()];
                        self.weighted_need = Some(self.num_weighted(idx));
                        let mut iter = bucket.weighted();
                        if let Some(elem) = iter.next() {
                            // increment self.got
                            self.weighted_got.as_mut().map(|g| *g += 1);
                            self.weighted_iter = Some(Box::new(iter));
                            return Some(elem);
                        }
                    } else if let Some(idx) = self.swamp_buckets.next() {
                        // We've searched through all weighted buckets, and found nothing
                        // Now take element from swamp buckets
                        let bucket = &self.table.buckets[idx.get()];
                        let mut iter = bucket.swamp();
                        if let Some(elem) = iter.next() {
                            // TODO: increment self.got?
                            self.swamp_iter = Some(Box::new(iter));
                            return Some(elem);
                        }
                    } else {
                        return None;
                    }
                }
                // we're iterating through weighted
                Some(got) => {
                    let need = self
                        .weighted_need
                        .expect("self.need defined when self.got defined");
                    if got < need {
                        let mut iter = self
                            .weighted_iter
                            .as_deref_mut()
                            .expect("iterating through weighted");

                        // If element is found – return it
                        if let Some(elem) = iter.next() {
                            // increment self.got
                            self.weighted_got.as_mut().map(|g| *g += 1);
                            return Some(elem);
                        } else if let Some(idx) = self.weighted_buckets.next() {
                            // If no elements in the current weighted bucket, go to the next
                            let bucket = &self.table.buckets[idx.get()];
                            self.weighted_need = Some(self.num_weighted(idx));
                            self.weighted_iter = Some(Box::new(bucket.weighted()));
                        } else {
                            // No more weighted buckets left
                            self.weighted_got = None;
                            self.weighted_need = None;
                            self.weighted_iter = None;
                        }

                        /*
                        else if let Some(mut iter) = self.swamp_iter.as_deref_mut() {
                            // No more weighted buckets left, take elem from current swamp


                            if let Some(elem) = iter.next() {
                                return Some(elem);
                            }
                        } else if let Some(idx) = self.swamp_buckets.next() {
                            // No elements in the current swamp bucket, go to the next
                            let bucket = &self.table.buckets[idx.get()];
                            self.swamp_iter = Some(Box::new(bucket.swamp()));
                        }
                         */
                    }
                }
            }
        }

        if let Some(i) = self.weighted_buckets.next() {
            let bucket = &self.table.buckets[i.get()];
            let mut peers = bucket.iter();
            // 1. Set need = num_weighted(bucket.distance)
            // 2. take `need` peers from weighted
            // 3. if got < need, go to the next weighted bucket
            // Go to 1.
            // 4. if got == need, take num_swamp peers from swamp
            // Go to 1.

            let mut iter = bucket.weighted();
            let need = Self::num_weighted(bucket.index);
            let got = iter.by_ref().take(need);
            if got.count() < need {}

            // peers.sort_by(|a, b| {
            //     Ord::cmp(
            //         &self.target.as_ref().distance(a.as_ref()),
            //         &self.target.as_ref().distance(b.as_ref())
            //     )
            // });
            self.iter = Some(peers.into_iter());
        }

        None
    }
}
