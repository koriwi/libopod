use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

use rusqlite::{Connection, OpenFlags};

use crate::{error::io_error, Error, Result};

const SQLITE_HEADER_BYTES: usize = 100;

/// One member of an iTunes `SQLite` `.itlp` library.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SqliteLibraryFile {
    Library,
    Locations,
    Dynamic,
    Extras,
    Genius,
}

impl SqliteLibraryFile {
    /// All required members in stable installation order.
    pub const ALL: [Self; 5] = [
        Self::Library,
        Self::Locations,
        Self::Dynamic,
        Self::Extras,
        Self::Genius,
    ];

    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Library => "Library.itdb",
            Self::Locations => "Locations.itdb",
            Self::Dynamic => "Dynamic.itdb",
            Self::Extras => "Extras.itdb",
            Self::Genius => "Genius.itdb",
        }
    }
}

/// Redacted `SQLite` header and integrity information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteDatabaseInfo {
    pub file: SqliteLibraryFile,
    pub bytes: u64,
    pub page_size: u32,
    pub page_count: u32,
    pub write_version: u8,
    pub read_version: u8,
    pub schema_cookie: u32,
    pub schema_format: u32,
    pub user_version: u32,
    pub sqlite_version_number: u32,
    pub integrity_ok: bool,
}

pub(crate) fn inspect_sqlite_database(
    path: &Path,
    file: SqliteLibraryFile,
) -> Result<SqliteDatabaseInfo> {
    let metadata =
        fs::metadata(path).map_err(|source| io_error("inspect SQLite file", path, source))?;
    if metadata.len() < SQLITE_HEADER_BYTES as u64 {
        return malformed("file is shorter than the 100-byte SQLite header");
    }

    let mut header = [0_u8; SQLITE_HEADER_BYTES];
    File::open(path)
        .map_err(|source| io_error("open SQLite file", path, source))?
        .read_exact(&mut header)
        .map_err(|source| io_error("read SQLite header", path, source))?;
    if &header[..16] != b"SQLite format 3\0" {
        return malformed("invalid SQLite 3 magic");
    }

    let encoded_page_size = be_u16(&header, 16)?;
    let page_size = if encoded_page_size == 1 {
        65_536
    } else {
        u32::from(encoded_page_size)
    };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return malformed("invalid SQLite page size");
    }
    let page_count = be_u32(&header, 28)?;
    let minimum_bytes = u64::from(page_count)
        .checked_mul(u64::from(page_size))
        .ok_or_else(|| malformed_error("page count overflows the file size"))?;
    if minimum_bytes > metadata.len() {
        return malformed("declared pages extend beyond the file");
    }

    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| Error::Sqlite {
        operation: "open read-only database",
        path: path.to_path_buf(),
        source,
    })?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|source| Error::Sqlite {
            operation: "PRAGMA integrity_check",
            path: path.to_path_buf(),
            source,
        })?;

    Ok(SqliteDatabaseInfo {
        file,
        bytes: metadata.len(),
        page_size,
        page_count,
        write_version: header[18],
        read_version: header[19],
        schema_cookie: be_u32(&header, 40)?,
        schema_format: be_u32(&header, 44)?,
        user_version: be_u32(&header, 60)?,
        sqlite_version_number: be_u32(&header, 96)?,
        integrity_ok: integrity == "ok",
    })
}

fn be_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| malformed_error("truncated SQLite u16"))?;
    Ok(u16::from_be_bytes([value[0], value[1]]))
}

fn be_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| malformed_error("truncated SQLite u32"))?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

fn malformed<T>(reason: &str) -> Result<T> {
    Err(malformed_error(reason))
}

fn malformed_error(reason: &str) -> Error {
    Error::Malformed {
        format: "SQLite database",
        offset: 0,
        reason: reason.to_owned(),
    }
}
