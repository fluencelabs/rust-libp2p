use super::*;

/// A `PendingNode` is a `Node` that is pending insertion into a `KBucket`.
#[derive(Debug, Clone)]
pub struct PendingNode<TKey, TVal> {
    /// The pending node to insert.
    pub(crate) node: Node<TKey, TVal>,

    /// The status of the pending node.
    pub(crate) status: NodeStatus,

    /// The instant at which the pending node is eligible for insertion into a bucket.
    pub(crate) replace: Instant,
}

/// The status of a node in a bucket.
///
/// The status of a node in a bucket together with the time of the
/// last status change determines the position of the node in a
/// bucket.
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum NodeStatus {
    /// The node is considered connected.
    Connected,
    /// The node is considered disconnected.
    Disconnected
}

impl<TKey, TVal> PendingNode<TKey, TVal> {
    pub fn key(&self) -> &TKey {
        &self.node.key
    }

    pub fn status(&self) -> NodeStatus {
        self.status
    }

    pub fn value_mut(&mut self) -> &mut TVal {
        &mut self.node.value
    }

    pub fn is_ready(&self) -> bool {
        Instant::now() >= self.replace
    }

    pub fn set_ready_at(&mut self, t: Instant) {
        self.replace = t;
    }
}

/// A `Node` in a bucket, representing a peer participating
/// in the Kademlia DHT together with an associated value (e.g. contact
/// information).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node<TKey, TVal> {
    /// The key of the node, identifying the peer.
    pub key: TKey,
    /// The associated value.
    pub value: TVal,
}

/// The position of a node in a `KBucket`, i.e. a non-negative integer
/// in the range `[0, K_VALUE)`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position(pub(crate) usize);

/// The result of inserting an entry into a bucket.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertResult<TKey> {
    /// The entry has been successfully inserted.
    Inserted,
    /// The entry is pending insertion because the relevant bucket is currently full.
    /// The entry is inserted after a timeout elapsed, if the status of the
    /// least-recently connected (and currently disconnected) node in the bucket
    /// is not updated before the timeout expires.
    Pending {
        /// The key of the least-recently connected entry that is currently considered
        /// disconnected and whose corresponding peer should be checked for connectivity
        /// in order to prevent it from being evicted. If connectivity to the peer is
        /// re-established, the corresponding entry should be updated with
        /// [`NodeStatus::Connected`].
        disconnected: TKey
    },
    /// The entry was not inserted because the relevant bucket is full.
    Full
}

/// The result of applying a pending node to a bucket, possibly
/// replacing an existing node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedPending<TKey, TVal> {
    /// The key of the inserted pending node.
    pub inserted: Node<TKey, TVal>,
    /// The node that has been evicted from the bucket to make room for the
    /// pending node, if any.
    pub evicted: Option<Node<TKey, TVal>>
}

pub struct Iter<'a, TKey, TVal>(dyn std::iter::Iterator<Item=(&'a Node<TKey, TVal>, NodeStatus)>);

pub trait Bucketable {
    type TKey: AsRef<KeyBytes> + Clone;
    type TVal: Clone;

    /// Returns a reference to the pending node of the bucket, if there is any.
    fn pending(&self) -> Option<&PendingNode<Self::TKey, Self::TVal>>;

    /// Returns a mutable reference to the pending node of the bucket, if there is any.
    fn pending_mut(&mut self) -> Option<&mut PendingNode<Self::TKey, Self::TVal>>;


    /// Returns a reference to the pending node of the bucket, if there is any
    /// with a matching key.
    fn as_pending(&self, key: &Self::TKey) -> Option<&PendingNode<Self::TKey, Self::TVal>>;

    /// Returns an iterator over the nodes in the bucket, together with their status.
    fn iter(&self) -> Iter<Self::TKey, Self::TVal>;

    /// Inserts the pending node into the bucket, if its timeout has elapsed,
    /// replacing the least-recently connected node.
    ///
    /// If a pending node has been inserted, its key is returned together with
    /// the node that was replaced. `None` indicates that the nodes in the
    /// bucket remained unchanged.
    fn apply_pending(&mut self) -> Option<AppliedPending<Self::TKey, Self::TVal>>;

    /// Updates the status of the pending node, if any.
    fn update_pending(&mut self, status: NodeStatus);

    /// Updates the status of the node referred to by the given key, if it is
    /// in the bucket.
    fn update(&mut self, key: &Self::TKey, status: NodeStatus);

    /// Inserts a new node into the bucket with the given status.
    ///
    /// The status of the node to insert determines the result as follows:
    ///
    ///   * `NodeStatus::Connected`: If the bucket is full and either all nodes are connected
    ///     or there is already a pending node, insertion fails with `InsertResult::Full`.
    ///     If the bucket is full but at least one node is disconnected and there is no pending
    ///     node, the new node is inserted as pending, yielding `InsertResult::Pending`.
    ///     Otherwise the bucket has free slots and the new node is added to the end of the
    ///     bucket as the most-recently connected node.
    ///
    ///   * `NodeStatus::Disconnected`: If the bucket is full, insertion fails with
    ///     `InsertResult::Full`. Otherwise the bucket has free slots and the new node
    ///     is inserted at the position preceding the first connected node,
    ///     i.e. as the most-recently disconnected node. If there are no connected nodes,
    ///     the new node is added as the last element of the bucket.
    ///
    fn insert(&mut self, node: Node<Self::TKey, Self::TKey>, status: NodeStatus) -> InsertResult<Self::TKey>;

    /// Returns the status of the node at the given position.
    fn status(&self, pos: Position) -> NodeStatus;

    /// Checks whether the given position refers to a connected node.
    fn is_connected(&self, pos: Position) -> bool;

    /// Gets the number of entries currently in the bucket.
    fn num_entries(&self) -> usize;

    /// Gets the number of entries in the bucket that are considered connected.
    fn num_connected(&self) -> usize;

    /// Gets the number of entries in the bucket that are considered disconnected.
    fn num_disconnected(&self) -> usize;

    /// Gets the position of an node in the bucket.
    fn position(&self, key: &Self::TKey) -> Option<Position>;

    /// Returns a reference to a node in the bucket.
    ///
    /// Returns `None` if the given key does not refer to a node in the
    /// bucket.
    fn get(&self, key: &Self::TKey) -> Option<&Node<Self::TKey, Self::TVal>>;

    /// Gets a mutable reference to the node identified by the given key.
    ///
    /// Returns `None` if the given key does not refer to a node in the
    /// bucket.
    fn get_mut(&mut self, key: &Self::TKey) -> Option<&mut Node<Self::TKey, Self::TVal>>;
}
