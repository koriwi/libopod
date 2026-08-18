mod cbk;
mod cdb;
mod cdb_add;
mod cdb_edit;
mod classic;

pub use cbk::CbkInfo;
pub use cdb::{CdbDatasetInfo, CdbInfo};
pub use classic::parse_library;

pub(crate) use cbk::{build_hashab_cbk, verify_cbk};
pub(crate) use cdb::inspect_cdb;
pub(crate) use cdb_add::{add_track_to_cdb, CdbArtworkLink, CdbTrackAddition};
pub(crate) use cdb_edit::remove_tracks_from_cdb;
