use std::{env, process::ExitCode};

use libopod::Device;

fn main() -> ExitCode {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "opod-inspect".to_owned());
    let Some(mount) = arguments.next() else {
        eprintln!("usage: {program} MOUNT_ROOT");
        return ExitCode::from(2);
    };
    if arguments.next().is_some() {
        eprintln!("usage: {program} MOUNT_ROOT");
        return ExitCode::from(2);
    }

    match inspect(mount) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("inspection failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn inspect(mount: std::ffi::OsString) -> libopod::Result<()> {
    let device = Device::open(mount)?;
    let evidence = device.evidence();
    println!(
        "profile: {}",
        device
            .profile()
            .map_or("unresolved", |profile| profile.display_name())
    );
    println!(
        "product serial: {}",
        presence(evidence.has_product_serial())
    );
    println!(
        "FireWire GUID: {} (value redacted)",
        presence(evidence.has_firewire_guid())
    );
    if let Some(value) = evidence.sqlite_db() {
        println!("SQLite capability: {} ({:?})", value.value, value.source);
    }
    if let Some(value) = evidence.sparse_artwork() {
        println!("sparse artwork: {} ({:?})", value.value, value.source);
    }

    if let Some(library) = device.library() {
        println!("music tracks: {}", library.track_count());
    } else {
        println!("music tracks: unavailable for this profile");
    }

    let inspection = device.inspection();
    println!(
        "iTunesDB marker/file: {}",
        presence(inspection.itunes_db_present)
    );
    if let Some(cdb) = &inspection.cdb {
        let kinds: Vec<String> = cdb
            .datasets
            .iter()
            .map(|item| item.kind.to_string())
            .collect();
        println!(
            "iTunesCDB: {} bytes, version {}, scheme {}, indicator {}, datasets [{}]",
            cdb.physical_bytes,
            cdb.version,
            cdb.checksum_scheme,
            cdb.checksum_indicator,
            kinds.join(", ")
        );
        println!("  HASHAB signature: {:?}", cdb.hashab_signature_status);
    } else {
        println!("iTunesCDB: absent");
    }

    for database in &inspection.sqlite_databases {
        println!(
            "SQLite {}: {} bytes, {} pages x {}, integrity {}",
            database.file.file_name(),
            database.bytes,
            database.page_count,
            database.page_size,
            if database.integrity_ok {
                "ok"
            } else {
                "FAILED"
            }
        );
    }
    if let Some(cbk) = &inspection.cbk {
        println!(
            "Locations CBK: {} blocks x {}, scheme {}, digests {}",
            cbk.block_count,
            cbk.block_size,
            cbk.checksum_scheme,
            if cbk.digests_match() { "ok" } else { "FAILED" }
        );
        println!(
            "  HASHAB signature: {}",
            verification_label(cbk.hashab_signature_matches)
        );
    }
    if let Some(artwork) = &inspection.artwork_database {
        println!(
            "ArtworkDB: {} bytes, {} datasets, next image ID {}",
            artwork.bytes, artwork.declared_children, artwork.next_image_id
        );
        for dataset in &artwork.datasets {
            let magic = String::from_utf8_lossy(&dataset.list_magic);
            println!(
                "  dataset {}: {} items ({magic})",
                dataset.kind, dataset.item_count
            );
        }
    }
    for frames in &inspection.artwork_frames {
        println!(
            "artwork F{}_1.ithmb: {} slots x {}, remainder {}",
            frames.format_id, frames.complete_slots, frames.slot_bytes, frames.remainder_bytes
        );
    }
    Ok(())
}

const fn verification_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "ok",
        Some(false) => "FAILED",
        None => "not checked",
    }
}

const fn presence(value: bool) -> &'static str {
    if value {
        "present"
    } else {
        "absent"
    }
}
