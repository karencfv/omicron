// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Example values for sled-agent-zone-images types.
//!
//! This crate provides helpers for generating example values for zone image
//! resolver types.

use std::{collections::BTreeSet, fs, io, sync::LazyLock};

use camino::{Utf8Path, Utf8PathBuf};
use camino_tempfile_ext::{
    fixture::{ChildPath, FixtureError, FixtureKind},
    prelude::*,
};
use iddqd::{IdOrdItem, IdOrdMap, id_upcast};
use omicron_common::update::{
    MupdateOverrideInfo, OmicronZoneFileMetadata, OmicronZoneManifest,
    OmicronZoneManifestSource,
};
use omicron_uuid_kinds::{InternalZpoolUuid, MupdateOverrideUuid, MupdateUuid};
use sha2::{Digest, Sha256};
use sled_agent_types::zone_images::{
    ArcIoError, ArcSerdeJsonError, ArtifactReadResult,
    InstallMetadataReadError, ZoneManifestArtifactResult,
    ZoneManifestArtifactsResult,
};
use tufaceous_artifact::ArtifactHash;

pub struct OverridePaths {
    pub install_dataset: Utf8PathBuf,
    pub zones_json: Utf8PathBuf,
    pub mupdate_override_json: Utf8PathBuf,
}

impl OverridePaths {
    fn for_uuid(uuid: InternalZpoolUuid) -> Self {
        let install_dataset =
            Utf8PathBuf::from(format!("pool/int/{uuid}/install"));
        let zones_json = install_dataset.join(OmicronZoneManifest::FILE_NAME);
        let mupdate_override_json =
            install_dataset.join(MupdateOverrideInfo::FILE_NAME);
        Self { install_dataset, zones_json, mupdate_override_json }
    }
}

pub const BOOT_UUID: InternalZpoolUuid =
    InternalZpoolUuid::from_u128(0xd3e7205d_4efe_493b_ac5e_9175584907cd);
pub static BOOT_PATHS: LazyLock<OverridePaths> =
    LazyLock::new(|| OverridePaths::for_uuid(BOOT_UUID));

pub const NON_BOOT_UUID: InternalZpoolUuid =
    InternalZpoolUuid::from_u128(0x4854189f_b290_47cd_b076_374d0e1748ec);
pub static NON_BOOT_PATHS: LazyLock<OverridePaths> =
    LazyLock::new(|| OverridePaths::for_uuid(NON_BOOT_UUID));

pub const NON_BOOT_2_UUID: InternalZpoolUuid =
    InternalZpoolUuid::from_u128(0x72201e1e_9fee_4231_81cd_4e2d514cb632);
pub static NON_BOOT_2_PATHS: LazyLock<OverridePaths> =
    LazyLock::new(|| OverridePaths::for_uuid(NON_BOOT_2_UUID));

pub const NON_BOOT_3_UUID: InternalZpoolUuid =
    InternalZpoolUuid::from_u128(0xd0d04947_93c5_40fd_97ab_4648b8cc28d6);
pub static NON_BOOT_3_PATHS: LazyLock<OverridePaths> =
    LazyLock::new(|| OverridePaths::for_uuid(NON_BOOT_3_UUID));

/// Context for writing out fake zones to install dataset directories.
///
/// The tests in this module ensure that the override JSON's list of zones
/// matches the zone files on disk.
#[derive(Clone, Debug)]
pub struct WriteInstallDatasetContext {
    pub zones: IdOrdMap<ZoneContents>,
    pub mupdate_id: MupdateUuid,
    pub mupdate_override_uuid: MupdateOverrideUuid,
    pub write_zone_manifest_to_disk: bool,
}

impl WriteInstallDatasetContext {
    /// Initializes a new context with a couple of zones and no known
    /// errors.
    pub fn new_basic() -> Self {
        Self {
            zones: [
                ZoneContents::new("zone1.tar.gz", b"zone1"),
                ZoneContents::new("zone2.tar.gz", b"zone2"),
                ZoneContents::new("zone3.tar.gz", b"zone3"),
                ZoneContents::new("zone4.tar.gz", b"zone4"),
                ZoneContents::new("zone5.tar.gz", b"zone5"),
            ]
            .into_iter()
            .collect(),
            mupdate_id: MupdateUuid::new_v4(),
            mupdate_override_uuid: MupdateOverrideUuid::new_v4(),
            write_zone_manifest_to_disk: true,
        }
    }

    /// Makes a number of error cases for testing.
    pub fn make_error_cases(&mut self) {
        // zone1.tar.gz is valid.
        // For zone2.tar.gz, change the size.
        self.zones.get_mut("zone2.tar.gz").unwrap().json_size = 1024;
        // For zone3.tar.gz, change the hash.
        self.zones.get_mut("zone3.tar.gz").unwrap().json_hash =
            ArtifactHash([0; 32]);
        // Don't write out zone4 but include it in the JSON.
        self.zones.get_mut("zone4.tar.gz").unwrap().write_to_disk = false;
        // Write out zone5 but don't include it in the JSON.
        self.zones.get_mut("zone5.tar.gz").unwrap().include_in_json = false;
    }

    /// Set to false to not write out the zone manifest to disk.
    pub fn write_zone_manifest_to_disk(&mut self, write: bool) {
        self.write_zone_manifest_to_disk = write;
    }

    /// Returns the override information for the mupdate.
    pub fn override_info(&self) -> MupdateOverrideInfo {
        MupdateOverrideInfo {
            mupdate_uuid: self.mupdate_override_uuid,
            // The hash IDs are not used for validation, so leave this
            // empty.
            hash_ids: BTreeSet::new(),
        }
    }

    pub fn zone_manifest(&self) -> OmicronZoneManifest {
        let source = if self.write_zone_manifest_to_disk {
            OmicronZoneManifestSource::Installinator {
                mupdate_id: self.mupdate_id,
            }
        } else {
            OmicronZoneManifestSource::SledAgent
        };
        OmicronZoneManifest {
            source,
            zones: self
                .zones
                .iter()
                .filter_map(|zone| {
                    zone.include_in_json.then(|| OmicronZoneFileMetadata {
                        file_name: zone.file_name.clone(),
                        file_size: zone.json_size,
                        hash: zone.json_hash,
                    })
                })
                .collect(),
        }
    }

    /// Returns the expected result of writing the zone manifest, taking into
    /// account mismatches, etc.
    pub fn expected_result(
        &self,
        dir: &Utf8Path,
    ) -> ZoneManifestArtifactsResult {
        let manifest = self.zone_manifest();
        let data = self
            .zones
            .iter()
            .filter_map(|zone| {
                // Currently, zone files not present in the JSON aren't
                // reported at all.
                //
                // XXX: should they be?
                zone.include_in_json.then(|| zone.expected_result(dir))
            })
            .collect();
        ZoneManifestArtifactsResult { manifest, data }
    }

    /// Writes the context to a directory, returning the JSON that was
    /// written out.
    pub fn write_to(&self, dir: &ChildPath) -> Result<(), FixtureError> {
        for zone in &self.zones {
            if zone.write_to_disk {
                dir.child(&zone.file_name).write_binary(&zone.contents)?;
            }
        }

        if self.write_zone_manifest_to_disk {
            let manifest = self.zone_manifest();
            let json = serde_json::to_string(&manifest).map_err(|e| {
                FixtureError::new(FixtureKind::WriteFile).with_source(e)
            })?;
            // No need to create intermediate directories with
            // camino-tempfile-ext.
            dir.child(OmicronZoneManifest::FILE_NAME).write_str(&json)?;
        }

        let info = self.override_info();
        let json = serde_json::to_string(&info).map_err(|e| {
            FixtureError::new(FixtureKind::WriteFile).with_source(e)
        })?;
        dir.child(MupdateOverrideInfo::FILE_NAME).write_str(&json)?;

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ZoneContents {
    file_name: String,
    contents: Vec<u8>,
    // json_size and json_hash are stored separately, so tests can tweak
    // them before writing out the override info.
    json_size: u64,
    json_hash: ArtifactHash,
    write_to_disk: bool,
    include_in_json: bool,
}

impl ZoneContents {
    fn new(file_name: &str, contents: &[u8]) -> Self {
        let size = contents.len() as u64;
        let hash = compute_hash(contents);
        Self {
            file_name: file_name.to_string(),
            contents: contents.to_vec(),
            json_size: size,
            json_hash: hash,
            write_to_disk: true,
            include_in_json: true,
        }
    }

    fn expected_result(&self, dir: &Utf8Path) -> ZoneManifestArtifactResult {
        let status = if !self.write_to_disk {
            // Missing from the disk
            ArtifactReadResult::Error(ArcIoError::new(io::Error::new(
                io::ErrorKind::NotFound,
                "file not found",
            )))
        } else {
            let actual_size = self.contents.len() as u64;
            let actual_hash = compute_hash(&self.contents);
            if self.json_size != actual_size || self.json_hash != actual_hash {
                ArtifactReadResult::Mismatch { actual_size, actual_hash }
            } else {
                ArtifactReadResult::Valid
            }
        };

        ZoneManifestArtifactResult {
            file_name: self.file_name.clone(),
            path: dir.join(&self.file_name),
            expected_size: self.json_size,
            expected_hash: self.json_hash,
            status,
        }
    }
}

impl IdOrdItem for ZoneContents {
    type Key<'a> = &'a str;

    fn key(&self) -> Self::Key<'_> {
        &self.file_name
    }

    id_upcast!();
}

fn compute_hash(contents: &[u8]) -> ArtifactHash {
    let hash = Sha256::digest(contents);
    ArtifactHash(hash.into())
}

pub fn dataset_missing_error(dir_path: &Utf8Path) -> InstallMetadataReadError {
    InstallMetadataReadError::DatasetDirMetadata {
        dataset_dir: dir_path.to_owned(),
        error: ArcIoError::new(io::Error::from(io::ErrorKind::NotFound)),
    }
}

pub fn dataset_not_dir_error(dir_path: &Utf8Path) -> InstallMetadataReadError {
    // A `FileType` must unfortunately be retrieved from disk -- can't
    // create a new one in-memory. We assume that `dir.path()` passed in
    // actually has the described error condition.
    InstallMetadataReadError::DatasetNotDirectory {
        dataset_dir: dir_path.to_owned(),
        file_type: fs::symlink_metadata(dir_path)
            .expect("lstat on dir.path() succeeded")
            .file_type(),
    }
}

pub fn deserialize_error(
    dir_path: &Utf8Path,
    json_path: &Utf8Path,
    contents: &str,
) -> InstallMetadataReadError {
    InstallMetadataReadError::Deserialize {
        path: dir_path.join(json_path),
        contents: contents.to_owned(),
        error: ArcSerdeJsonError::new(
            serde_json::from_str::<MupdateOverrideInfo>(contents).unwrap_err(),
        ),
    }
}
