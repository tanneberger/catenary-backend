// Copyright Kyler Chin <kyler@catenarymaps.org>
// Catenary Transit Initiatives
// Attribution cannot be removed

/// This is the service definition. It looks a lot like a trait definition.
/// It defines one RPC, hello, which takes one arg, name, and returns a String.

#[tarpc::service]
pub trait AspenRpc {
    /// Returns a greeting for name.
    async fn hello(name: String) -> String;

    //maybesend gtfs rt?
    async fn from_alpenrose(
        chateau_id: String,
        realtime_feed_id: String,
        vehicles: Option<Vec<u8>>,
        trips: Option<Vec<u8>>,
        alerts: Option<Vec<u8>>,
        has_vehicles: bool,
        has_trips: bool,
        has_alerts: bool,
        vehicles_response_code: Option<u16>,
        trips_response_code: Option<u16>,
        alerts_response_code: Option<u16>,
        time_of_submission_ms: u64,
    ) -> bool;
}

use crate::ChateauDataNoGeometry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::IpAddr;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChateauMetadataZookeeper {
    pub worker_id: String,
    pub tailscale_ip: IpAddr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RealtimeFeedMetadataZookeeper {
    pub worker_id: String,
    pub tailscale_ip: IpAddr,
    pub chateau_id: String,
}

#[derive(Debug, Hash, Clone, Eq, PartialEq)]
pub struct ChateausLeaderHashMap {
    pub chateaus: BTreeMap<String, ChateauDataNoGeometry>,
}

#[derive(Clone)]
pub struct ProcessAlpenroseData {
    pub chateau_id: String,
    pub realtime_feed_id: String,
    pub vehicles: Option<Vec<u8>>,
    pub trips: Option<Vec<u8>>,
    pub alerts: Option<Vec<u8>>,
    pub has_vehicles: bool,
    pub has_trips: bool,
    pub has_alerts: bool,
    pub vehicles_response_code: Option<u16>,
    pub trips_response_code: Option<u16>,
    pub alerts_response_code: Option<u16>,
    pub time_of_submission_ms: u64,
}
