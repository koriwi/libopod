mod cbk;
mod cdb;
mod cdb_add;
mod cdb_edit;
mod classic;
mod classic_edit;

// Round-trip differ harness helpers, shared with the edit test modules
// (crate-internal, test builds only).
#[cfg(test)]
pub(crate) use cbk::verify_cbk as cdb_cbk_verify;
#[cfg(test)]
pub(crate) use cdb::decode_payload as cdb_decode_payload;
#[cfg(test)]
pub(crate) use cdb_edit::{assert_retained_chunks_preserved, cdb_track_pids};

pub use cbk::CbkInfo;
pub use cdb::{CdbDatasetInfo, CdbInfo};
pub use classic::parse_library;
pub(crate) use classic_edit::{
    add_track as add_classic_track, remove_tracks as remove_classic_tracks,
};

pub(crate) use cbk::{build_hashab_cbk, verify_cbk};
pub(crate) use cdb::inspect_cdb;
pub(crate) use cdb_add::{add_track_to_cdb, CdbArtworkLink, CdbTrackAddition};
pub(crate) use cdb_edit::remove_tracks_from_cdb;
