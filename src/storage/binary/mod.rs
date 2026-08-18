mod cbk;
mod cdb;
mod cdb_add;
mod cdb_edit;
mod classic;
mod classic_edit;

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
