use crate::{
    artwork::{inspect_artwork_db, inspect_frame_file},
    fs::read_limited,
    storage::{
        binary::{inspect_cdb, verify_cbk},
        sqlite::inspect_sqlite_database,
    },
    ArtworkDatabaseInfo, ArtworkFrameInfo, CbkInfo, CdbInfo, IpodPath, MountRoot, Result,
    SqliteDatabaseInfo, SqliteLibraryFile,
};

use super::DeviceProfile;

const MAX_CDB_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LOCATIONS_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CBK_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARTWORK_DB_BYTES: u64 = 256 * 1024 * 1024;
const ITLP: &str = "iPod_Control/iTunes/iTunes Library.itlp";

/// Redacted, read-only structural inspection results.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceInspection {
    pub itunes_db_present: bool,
    pub cdb: Option<CdbInfo>,
    pub sqlite_databases: Vec<SqliteDatabaseInfo>,
    pub cbk: Option<CbkInfo>,
    pub artwork_database: Option<ArtworkDatabaseInfo>,
    pub artwork_frames: Vec<ArtworkFrameInfo>,
}

impl DeviceInspection {
    pub(crate) fn read_from(
        mount: &MountRoot,
        profile: Option<&DeviceProfile>,
        firewire_guid: Option<&[u8; 8]>,
    ) -> Result<Self> {
        let mut inspection = Self {
            itunes_db_present: contains(mount, "iPod_Control/iTunes/iTunesDB")?,
            ..Self::default()
        };

        if let Some(path) = existing(mount, "iPod_Control/iTunes/iTunesCDB")? {
            let bytes = read_limited(&path, MAX_CDB_BYTES, "iTunesCDB")?;
            inspection.cdb = Some(inspect_cdb(&bytes, firewire_guid)?);
        }

        for file in [
            SqliteLibraryFile::Library,
            SqliteLibraryFile::Locations,
            SqliteLibraryFile::Dynamic,
            SqliteLibraryFile::Extras,
            SqliteLibraryFile::Genius,
        ] {
            let relative = format!("{ITLP}/{}", file.file_name());
            if let Some(path) = existing(mount, &relative)? {
                inspection
                    .sqlite_databases
                    .push(inspect_sqlite_database(&path, file)?);
            }
        }

        let locations = existing(mount, &format!("{ITLP}/Locations.itdb"))?;
        let cbk = existing(mount, &format!("{ITLP}/Locations.itdb.cbk"))?;
        if let (Some(locations), Some(cbk)) = (locations, cbk) {
            let locations = read_limited(&locations, MAX_LOCATIONS_BYTES, "Locations.itdb")?;
            let cbk = read_limited(&cbk, MAX_CBK_BYTES, "Locations.itdb.cbk")?;
            inspection.cbk = Some(verify_cbk(&locations, &cbk, firewire_guid)?);
        }

        if let Some(path) = existing(mount, "iPod_Control/Artwork/ArtworkDB")? {
            let bytes = read_limited(&path, MAX_ARTWORK_DB_BYTES, "ArtworkDB")?;
            inspection.artwork_database = Some(inspect_artwork_db(&bytes)?);
        }

        if let Some(profile) = profile {
            for format in &profile.capabilities().artwork_formats {
                let relative = format!("iPod_Control/Artwork/F{}_1.ithmb", format.format_id);
                if let Some(path) = existing(mount, &relative)? {
                    inspection.artwork_frames.push(inspect_frame_file(
                        &path,
                        format.format_id,
                        format.slot_bytes,
                    )?);
                }
            }
        }

        Ok(inspection)
    }
}

fn contains(mount: &MountRoot, relative: &str) -> Result<bool> {
    mount.contains(&IpodPath::new(relative)?)
}

fn existing(mount: &MountRoot, relative: &str) -> Result<Option<std::path::PathBuf>> {
    let relative = IpodPath::new(relative)?;
    if !mount.contains(&relative)? {
        return Ok(None);
    }
    mount.resolve_existing(&relative).map(Some)
}
