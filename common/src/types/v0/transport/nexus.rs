use super::*;

use crate::{
    transport_api::{ReplyError, ResourceKind},
    types::v0::store::{
        definitions::ObjectKey, nexus_child::NexusChild, nexus_persistence::NexusInfoKey,
    },
};
use serde::{Deserialize, Serialize};
use std::{convert::TryFrom, fmt::Debug, ops::RangeInclusive};
use strum_macros::{EnumString, ToString};

/// Volume Nexuses
///
/// Get all the nexuses with a filter selection
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct GetNexuses {
    /// Filter request
    pub filter: Filter,
}

/// Nexus information
#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Nexus {
    /// id of the io-engine instance
    pub node: NodeId,
    /// name of the nexus
    pub name: String,
    /// uuid of the nexus
    pub uuid: NexusId,
    /// size of the volume in bytes
    pub size: u64,
    /// current status of the nexus
    pub status: NexusStatus,
    /// array of children
    pub children: Vec<Child>,
    /// URI of the device for the volume (missing if not published).
    /// Missing property and empty string are treated the same.
    pub device_uri: String,
    /// number of active rebuild jobs
    pub rebuilds: u32,
    /// protocol used for exposing the nexus
    pub share: Protocol,
}
impl Nexus {
    /// Check if the nexus contains the provided `ChildUri`
    pub fn contains_child(&self, uri: &ChildUri) -> bool {
        self.children.iter().any(|c| &c.uri == uri)
    }
}

impl From<Nexus> for models::Nexus {
    fn from(src: Nexus) -> Self {
        models::Nexus::new(
            src.children,
            src.device_uri,
            src.node,
            src.rebuilds,
            src.share,
            src.size,
            src.status,
            src.uuid,
        )
    }
}

rpc_impl_string_uuid!(NexusId, "UUID of a nexus");

/// Nexus State information
#[derive(Serialize, Deserialize, Debug, Clone, EnumString, ToString, Eq, PartialEq)]
pub enum NexusStatus {
    /// Default Unknown state
    Unknown = 0,
    /// healthy and working
    Online = 1,
    /// not healthy but is able to serve IO (i.e. rebuild is in progress)
    Degraded = 2,
    /// broken and unable to serve IO
    Faulted = 3,
}
impl Default for NexusStatus {
    fn default() -> Self {
        Self::Unknown
    }
}
impl From<i32> for NexusStatus {
    fn from(src: i32) -> Self {
        match src {
            1 => Self::Online,
            2 => Self::Degraded,
            3 => Self::Faulted,
            _ => Self::Unknown,
        }
    }
}
impl From<NexusStatus> for models::NexusState {
    fn from(src: NexusStatus) -> Self {
        match src {
            NexusStatus::Unknown => Self::Unknown,
            NexusStatus::Online => Self::Online,
            NexusStatus::Degraded => Self::Degraded,
            NexusStatus::Faulted => Self::Faulted,
        }
    }
}

/// The protocol used to share the nexus.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, EnumString, ToString, Eq, PartialEq)]
#[strum(serialize_all = "camelCase")]
#[serde(rename_all = "camelCase")]
pub enum NexusShareProtocol {
    /// shared as NVMe-oF TCP
    Nvmf = 1,
    /// shared as iSCSI
    Iscsi = 2,
}

impl std::cmp::PartialEq<Protocol> for NexusShareProtocol {
    fn eq(&self, other: &Protocol) -> bool {
        &Protocol::from(*self) == other
    }
}
impl Default for NexusShareProtocol {
    fn default() -> Self {
        Self::Nvmf
    }
}
impl From<NexusShareProtocol> for Protocol {
    fn from(src: NexusShareProtocol) -> Self {
        match src {
            NexusShareProtocol::Nvmf => Self::Nvmf,
            NexusShareProtocol::Iscsi => Self::Iscsi,
        }
    }
}
impl From<models::NexusShareProtocol> for NexusShareProtocol {
    fn from(src: models::NexusShareProtocol) -> Self {
        match src {
            models::NexusShareProtocol::Nvmf => Self::Nvmf,
            models::NexusShareProtocol::Iscsi => Self::Iscsi,
        }
    }
}
impl TryFrom<Protocol> for NexusShareProtocol {
    type Error = String;

    fn try_from(value: Protocol) -> Result<Self, Self::Error> {
        match value {
            Protocol::None => Err(format!("Invalid protocol: {:?}", value)),
            Protocol::Nvmf => Ok(Self::Nvmf),
            Protocol::Iscsi => Ok(Self::Iscsi),
            Protocol::Nbd => Err(format!("Invalid protocol: {:?}", value)),
        }
    }
}

/// Create Nexus Request
#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateNexus {
    /// id of the io-engine instance
    pub node: NodeId,
    /// the nexus uuid will be set to this
    pub uuid: NexusId,
    /// size of the device in bytes
    pub size: u64,
    /// replica can be iscsi and nvmf remote targets or a local spdk bdev
    /// (i.e. bdev:///name-of-the-bdev).
    ///
    /// uris to the targets we connect to
    pub children: Vec<NexusChild>,
    /// Managed by our control plane
    pub managed: bool,
    /// Volume which owns this nexus, if any
    pub owner: Option<VolumeId>,
    /// Nexus Nvmf Configuration
    pub config: Option<NexusNvmfConfig>,
}

/// Nvmf Controller Id Range
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NvmfControllerIdRange(std::ops::RangeInclusive<u16>);
impl NvmfControllerIdRange {
    /// create `Self` with a random minimum controller id
    pub fn random_min() -> Self {
        let min = *Self::controller_id_range().start();
        let max = *Self::controller_id_range().end();
        let rand_min = u16::min(rand::random::<u16>() + min, max);
        Self(rand_min ..= max)
    }
    /// minimum controller id
    pub fn min(&self) -> &u16 {
        self.0.start()
    }
    /// maximum controller id
    pub fn max(&self) -> &u16 {
        self.0.end()
    }
    fn controller_id_range() -> std::ops::RangeInclusive<u16> {
        const MIN_CONTROLLER_ID: u16 = 1;
        const MAX_CONTROLLER_ID: u16 = 0xffef;
        MIN_CONTROLLER_ID ..= MAX_CONTROLLER_ID
    }
    /// create a new NvmfControllerIdRange using provided range
    pub fn new(start: u16, end: u16) -> Result<Self, ReplyError> {
        if NvmfControllerIdRange::controller_id_range().contains(&start)
            && NvmfControllerIdRange::controller_id_range().contains(&end)
            && start < end
        {
            Ok(NvmfControllerIdRange(RangeInclusive::new(start, end)))
        } else {
            Err(ReplyError::invalid_argument(
                ResourceKind::Nexus,
                "nvmf_controller_id_range",
                format!(
                    "{}, {} values don't fall in controller id range",
                    start, end
                ),
            ))
        }
    }
}
impl Default for NvmfControllerIdRange {
    fn default() -> Self {
        Self(Self::controller_id_range())
    }
}

/// Nexus Nvmf target configuration
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NexusNvmfConfig {
    /// limits the controller id range
    controller_id_range: NvmfControllerIdRange,
    /// persistent reservation key
    reservation_key: u64,
    /// preempts this reservation key
    preempt_reservation_key: Option<u64>,
}

impl NexusNvmfConfig {
    /// minimum controller id that can be used by the nvmf target
    pub fn min_cntl_id(&self) -> u16 {
        *self.controller_id_range.min()
    }
    /// maximum controller id that can be used by the nvmf target
    pub fn max_cntl_id(&self) -> u16 {
        *self.controller_id_range.max()
    }
    /// persistent reservation key
    pub fn resv_key(&self) -> u64 {
        self.reservation_key
    }
    /// reservation key to be preempted
    pub fn preempt_key(&self) -> u64 {
        self.preempt_reservation_key.unwrap_or_default()
    }
    /// create a new NexusNvmfConfig with the args
    pub fn new(
        controller_id_range: NvmfControllerIdRange,
        reservation_key: u64,
        preempt_reservation_key: Option<u64>,
    ) -> Self {
        Self {
            controller_id_range,
            reservation_key,
            preempt_reservation_key,
        }
    }
    /// get controller_id_range
    pub fn controller_id_range(&self) -> NvmfControllerIdRange {
        self.controller_id_range.clone()
    }
    /// get reservation_key
    pub fn reservation_key(&self) -> u64 {
        self.reservation_key
    }
    /// get preempt_reservation_key
    pub fn preempt_reservation_key(&self) -> Option<u64> {
        self.preempt_reservation_key
    }
}

impl Default for NexusNvmfConfig {
    fn default() -> Self {
        if std::env::var("TEST_NEXUS_NVMF_ANA_ENABLE").is_ok() {
            Self {
                controller_id_range: NvmfControllerIdRange::random_min(),
                reservation_key: 1,
                preempt_reservation_key: None,
            }
        } else {
            Self {
                controller_id_range: NvmfControllerIdRange::default(),
                reservation_key: 1,
                preempt_reservation_key: None,
            }
        }
    }
}

impl CreateNexus {
    /// Create new `Self` from the given parameters
    pub fn new(
        node: &NodeId,
        uuid: &NexusId,
        size: u64,
        children: &[NexusChild],
        managed: bool,
        owner: Option<&VolumeId>,
        config: Option<NexusNvmfConfig>,
    ) -> Self {
        Self {
            node: node.clone(),
            uuid: uuid.clone(),
            size,
            children: children.to_owned(),
            managed,
            owner: owner.cloned(),
            config,
        }
    }
    /// Name of the nexus.
    /// When part of a volume, it's set to its `VolumeId`. Otherwise it's set to its `NexusId`.
    pub fn name(&self) -> String {
        let name = self.owner.as_ref().map(|i| i.to_string());
        name.unwrap_or_else(|| self.uuid.to_string())
    }

    /// Return the key that should be used by the Io-Engine to persist the NexusInfo.
    pub fn nexus_info_key(&self) -> String {
        match &self.owner {
            Some(volume_id) => NexusInfoKey::new(&Some(volume_id.clone()), &self.uuid).key(),
            None => NexusInfoKey::new(&None, &self.uuid).key(),
        }
    }
}

/// Destroy Nexus Request.
#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DestroyNexus {
    /// The id of the io-engine instance.
    pub node: NodeId,
    /// The uuid of the nexus.
    pub uuid: NexusId,
    /// Nexus disowners.
    disowners: NexusOwners,
}
impl DestroyNexus {
    /// Create new `Self` from the given node and nexus id's.
    pub fn new(node: NodeId, uuid: NexusId) -> Self {
        Self {
            node,
            uuid,
            disowners: NexusOwners::None,
        }
    }
    /// Disown all owners.
    pub fn with_disown_all(mut self) -> Self {
        self.disowners = NexusOwners::All;
        self
    }
    /// Disown volume owner.
    pub fn with_disown(mut self, volume: &VolumeId) -> Self {
        self.disowners = NexusOwners::Volume(volume.clone());
        self
    }
    /// Return a reference to the disowners.
    pub fn disowners(&self) -> &NexusOwners {
        &self.disowners
    }
}

/// Nexus owners which is a volume, none or disowned by all.
#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
pub enum NexusOwners {
    #[default]
    None,
    Volume(VolumeId),
    All,
}
impl NexusOwners {
    /// Create a special `Self` that will disown all owners.
    pub fn new_disown_all() -> Self {
        Self::All
    }
    /// Set to disown all owners.
    pub fn with_disown_all(self) -> Self {
        Self::All
    }
    /// Disown all owners.
    pub fn disown_all(&self) -> bool {
        match self {
            NexusOwners::None => false,
            NexusOwners::Volume(_) => false,
            NexusOwners::All => true,
        }
    }
    /// Return the volume owner, if any
    pub fn volume(&self) -> Option<&VolumeId> {
        match self {
            NexusOwners::None => None,
            NexusOwners::Volume(volume) => Some(volume),
            NexusOwners::All => None,
        }
    }
}

impl From<Nexus> for DestroyNexus {
    fn from(nexus: Nexus) -> Self {
        Self {
            node: nexus.node,
            uuid: nexus.uuid,
            ..Default::default()
        }
    }
}

/// Share Nexus Request
#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShareNexus {
    /// id of the io-engine instance
    pub node: NodeId,
    /// uuid of the nexus
    pub uuid: NexusId,
    /// encryption key
    pub key: Option<String>,
    /// share protocol
    pub protocol: NexusShareProtocol,
}

impl From<(&Nexus, Option<String>, NexusShareProtocol)> for ShareNexus {
    fn from((nexus, key, protocol): (&Nexus, Option<String>, NexusShareProtocol)) -> Self {
        Self {
            node: nexus.node.clone(),
            uuid: nexus.uuid.clone(),
            key,
            protocol,
        }
    }
}
impl From<&Nexus> for UnshareNexus {
    fn from(from: &Nexus) -> Self {
        Self {
            node: from.node.clone(),
            uuid: from.uuid.clone(),
        }
    }
}
impl From<ShareNexus> for UnshareNexus {
    fn from(share: ShareNexus) -> Self {
        Self {
            node: share.node,
            uuid: share.uuid,
        }
    }
}

/// Unshare Nexus Request
#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnshareNexus {
    /// id of the io-engine instance
    pub node: NodeId,
    /// uuid of the nexus
    pub uuid: NexusId,
}
