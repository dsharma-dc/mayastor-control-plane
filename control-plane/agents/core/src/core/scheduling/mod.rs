pub(crate) mod nexus;
pub(crate) mod resources;
pub(crate) mod volume;

use crate::core::scheduling::{
    nexus::GetPersistedNexusChildrenCtx,
    resources::{ChildItem, NexusChildItem, PoolItem, ReplicaItem},
    volume::{GetSuitablePoolsContext, VolumeReplicasForNexusCtx},
};
use common_lib::types::v0::message_bus::PoolStatus;
use std::{cmp::Ordering, collections::HashMap, future::Future};

#[async_trait::async_trait(?Send)]
pub(crate) trait ResourceFilter: Sized {
    type Request;
    type Item;

    fn filter_iter(self, filter: fn(Self) -> Self) -> Self {
        filter(self)
    }
    async fn filter_iter_async<F, Fut>(self, filter: F) -> Self
    where
        F: Fn(Self) -> Fut,
        Fut: Future<Output = Self>,
    {
        filter(self).await
    }
    fn filter<F: FnMut(&Self::Request, &Self::Item) -> bool>(self, filter: F) -> Self;
    fn sort<F: FnMut(&Self::Item, &Self::Item) -> std::cmp::Ordering>(self, sort: F) -> Self;
    fn sort_ctx<F: FnMut(&Self::Request, &Self::Item, &Self::Item) -> std::cmp::Ordering>(
        self,
        _sort: F,
    ) -> Self {
        unimplemented!();
    }
    fn collect(self) -> Vec<Self::Item>;
    fn group_by<K, V, F: Fn(&Self::Request, &Vec<Self::Item>) -> HashMap<K, V>>(
        self,
        _group: F,
    ) -> HashMap<K, V> {
        unimplemented!();
    }
}

/// Filter nodes used for replica creation
pub(crate) struct NodeFilters {}
impl NodeFilters {
    /// Should only attempt to use online nodes
    pub(crate) fn online(_request: &GetSuitablePoolsContext, item: &PoolItem) -> bool {
        item.node.is_online()
    }
    /// Should only attempt to use allowed nodes (by the topology)
    pub(crate) fn allowed(request: &GetSuitablePoolsContext, item: &PoolItem) -> bool {
        request.allowed_nodes().is_empty() || request.allowed_nodes().contains(&item.pool.node)
    }
    /// Should only attempt to use nodes not currently used by the volume
    pub(crate) fn unused(request: &GetSuitablePoolsContext, item: &PoolItem) -> bool {
        let registry = request.registry();
        let used_nodes = registry.specs.get_volume_data_nodes(&request.uuid);
        !used_nodes.contains(&item.pool.node)
    }
}

/// Filter pools used for replica creation
pub(crate) struct PoolFilters {}
impl PoolFilters {
    /// Should only attempt to use pools with sufficient free space
    pub(crate) fn free_space(request: &GetSuitablePoolsContext, item: &PoolItem) -> bool {
        item.pool.free_space() > request.size
    }
    /// Should only attempt to use usable (not faulted) pools
    pub(crate) fn usable(_: &GetSuitablePoolsContext, item: &PoolItem) -> bool {
        item.pool.state != PoolStatus::Faulted && item.pool.state != PoolStatus::Unknown
    }
}

/// Sort the pools used for replica creation
pub(crate) struct PoolSorters {}
impl PoolSorters {
    /// Sort pools by their number of allocated replicas
    pub(crate) fn sort_by_replica_count(a: &PoolItem, b: &PoolItem) -> std::cmp::Ordering {
        a.pool.cmp(&b.pool)
    }
}

/// Sort the nexus children for removal when decreasing a volume's replica count
pub(crate) struct ChildSorters {}
impl ChildSorters {
    /// Sort replicas by their nexus child (state and rebuild progress)
    /// todo: should we use weights instead (like moac)?
    pub(crate) fn sort(a: &ReplicaItem, b: &ReplicaItem) -> std::cmp::Ordering {
        match Self::sort_by_child(a, b) {
            Ordering::Equal => {
                let childa_is_local = !a.spec().share.shared();
                let childb_is_local = !b.spec().share.shared();
                if childa_is_local == childb_is_local {
                    std::cmp::Ordering::Equal
                } else if childa_is_local {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Less
                }
            }
            ord => ord,
        }
    }
    fn sort_by_child(a: &ReplicaItem, b: &ReplicaItem) -> std::cmp::Ordering {
        // ANA not supported at the moment, so use only 1 child
        match a.child_spec() {
            None => {
                match b.child_spec() {
                    None => std::cmp::Ordering::Equal,
                    Some(_) => {
                        // prefer the replica that is not part of a nexus
                        std::cmp::Ordering::Greater
                    }
                }
            }
            Some(_) => {
                match b.child_spec() {
                    // prefer the replica that is not part of a nexus
                    None => std::cmp::Ordering::Less,
                    // compare the child states, and then the rebuild progress
                    Some(_) => match (a.child_state(), b.child_state()) {
                        (Some(a_state), Some(b_state)) => {
                            match a_state.state.partial_cmp(&b_state.state) {
                                None => a_state.rebuild_progress.cmp(&b_state.rebuild_progress),
                                Some(ord) => ord,
                            }
                        }
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    },
                }
            }
        }
    }
}

/// Filter the nexus children/replica candidates when creating a nexus
pub(crate) struct ChildInfoFilters {}
impl ChildInfoFilters {
    /// Should only allow healthy children
    pub(crate) fn healthy(request: &GetPersistedNexusChildrenCtx, item: &ChildItem) -> bool {
        // on first creation there is no nexus_info/child_info so all children are deemed healthy
        let first_create = request.nexus_info().is_none();
        first_create || item.info().as_ref().map(|i| i.healthy).unwrap_or(false)
    }
}

/// Filter the nexus children/replica candidates when creating a nexus
pub(crate) struct ReplicaFilters {}
impl ReplicaFilters {
    /// Should only allow children with corresponding online replicas
    pub(crate) fn online(_request: &GetPersistedNexusChildrenCtx, item: &ChildItem) -> bool {
        item.state().online()
    }

    /// Should only allow children with corresponding replicas with enough size
    pub(crate) fn size(request: &GetPersistedNexusChildrenCtx, item: &ChildItem) -> bool {
        item.state().size >= request.spec().size
    }
}

/// Sort the nexus replicas/children by preference when creating a nexus
pub(crate) struct ChildItemSorters {}
impl ChildItemSorters {
    /// Sort ChildItem's for volume nexus creation
    /// Prefer children local to where the nexus will be created
    pub(crate) fn sort_by_locality(
        request: &GetPersistedNexusChildrenCtx,
        a: &ChildItem,
        b: &ChildItem,
    ) -> std::cmp::Ordering {
        let a_is_local = &a.state().node == request.target_node();
        let b_is_local = &b.state().node == request.target_node();
        match (a_is_local, b_is_local) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (_, _) => std::cmp::Ordering::Equal,
        }
    }
}

/// Filter replicas when selecting the best candidates to add to a nexus
pub(crate) struct AddReplicaFilters {}
impl AddReplicaFilters {
    /// Should only allow children with corresponding online replicas
    pub(crate) fn online(_request: &VolumeReplicasForNexusCtx, item: &ChildItem) -> bool {
        item.state().online()
    }

    /// Should only allow children with corresponding replicas with enough size
    pub(crate) fn size(request: &VolumeReplicasForNexusCtx, item: &ChildItem) -> bool {
        item.state().size >= request.vol_spec().size
    }
}

/// Sort replicas to pick the best choice to add to a given nexus
pub(crate) struct AddReplicaSorters {}
impl AddReplicaSorters {
    /// Sorted by:
    /// 1. replicas local to the nexus
    /// 2. replicas which have not been marked as faulted by mayastor
    /// 3. replicas from pools with more free space
    pub(crate) fn sort(
        request: &VolumeReplicasForNexusCtx,
        a: &ChildItem,
        b: &ChildItem,
    ) -> std::cmp::Ordering {
        let a_is_local = a.state().node == request.nexus_spec().node;
        let b_is_local = b.state().node == request.nexus_spec().node;
        match (a_is_local, b_is_local) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (_, _) => {
                let a_healthy = a.info().as_ref().map(|i| i.healthy).unwrap_or(false);
                let b_healthy = b.info().as_ref().map(|i| i.healthy).unwrap_or(false);
                match (a_healthy, b_healthy) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    (_, _) => a.pool().free_space().cmp(&b.pool().free_space()),
                }
            }
        }
    }
}

/// Sort replicas to pick the best choice to remove from a given nexus
pub(crate) struct NexusChildSorter {}
impl NexusChildSorter {
    /// sort nexus children for removal
    /// remove "generic uri" children first (ie not spdk lvol replicas)
    /// then children with no state
    /// then children which are not local to the nexus
    pub(crate) fn sort(a: &NexusChildItem, b: &NexusChildItem) -> std::cmp::Ordering {
        match (a.replica(), b.replica()) {
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (_, _) => match (a.child_state(), b.child_state()) {
                (Some(a_status), Some(b_status)) => {
                    match a_status.state.partial_cmp(&b_status.state) {
                        None | Some(std::cmp::Ordering::Equal) => {
                            let a_is_local = a.replica().map(|spec| !spec.share.shared());
                            let b_is_local = b.replica().map(|spec| !spec.share.shared());
                            a_is_local.cmp(&b_is_local)
                        }
                        Some(ordering) => ordering,
                    }
                }
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            },
        }
    }
}
