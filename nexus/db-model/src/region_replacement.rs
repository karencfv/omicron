// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::impl_enum_type;
use crate::Region;
use crate::typed_uuid::DbTypedUuid;
use chrono::DateTime;
use chrono::Utc;
use nexus_db_schema::schema::region_replacement;
use omicron_uuid_kinds::VolumeKind;
use omicron_uuid_kinds::VolumeUuid;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

impl_enum_type!(
    RegionReplacementStateEnum:

    #[derive(Copy, Clone, Debug, AsExpression, FromSqlRow, Serialize, Deserialize, PartialEq)]
    pub enum RegionReplacementState;

    // Enum values
    Requested => b"requested"
    Allocating => b"allocating"
    Running => b"running"
    Driving => b"driving"
    ReplacementDone => b"replacement_done"
    Completing => b"completing"
    Complete => b"complete"
);

// FromStr impl required for use with clap (aka omdb)
impl std::str::FromStr for RegionReplacementState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "requested" => Ok(RegionReplacementState::Requested),
            "allocating" => Ok(RegionReplacementState::Allocating),
            "running" => Ok(RegionReplacementState::Running),
            "driving" => Ok(RegionReplacementState::Driving),
            "replacement_done" => Ok(RegionReplacementState::ReplacementDone),
            "complete" => Ok(RegionReplacementState::Complete),
            "completing" => Ok(RegionReplacementState::Completing),
            _ => Err(format!("unrecognized value {} for enum", s)),
        }
    }
}

/// Database representation of a Region replacement request.
///
/// This record stores the data related to the operations required for Nexus to
/// orchestrate replacing a region in a volume. It transitions through the
/// following states:
///
/// ```text
///     Requested   <--              ---
///                   |              |
///         |         |              |
///         v         |              |  responsibility of region
///                   |              |  replacement start saga
///     Allocating  --               |
///                                  |
///         |                        |
///         v                        ---
///                                  ---
///      Running    <--              |
///                   |              |
///         |         |              |
///         v         |              | responsibility of region
///                   |              | replacement drive saga
///      Driving    --               |
///                                  |
///         |                        |
///         v                        ---
///                                  ---
///  ReplacementDone  <--            |
///                     |            |
///         |           |            |
///         v           |            |
///                     |            | responsibility of region
///     Completing    --             | replacement finish saga
///                                  |
///         |                        |
///         v                        |
///                                  |
///     Complete                     ---
/// ```
///
/// which are captured in the RegionReplacementState enum. Annotated on the
/// right are which sagas are responsible for which state transitions. The state
/// transitions themselves are performed by these sagas and all involve a query
/// that:
///
///  - checks that the starting state (and other values as required) make sense
///  - updates the state while setting a unique operating_saga_id id (and any
///    other fields as appropriate)
///
/// As multiple background tasks will be waking up, checking to see what sagas
/// need to be triggered, and requesting that these region replacement sagas
/// run, this is meant to block multiple sagas from running at the same time in
/// an effort to cut down on interference - most will unwind at the first step
/// of performing this state transition instead of somewhere in the middle.
///
/// The correctness of a region replacement relies on certain operations
/// happening only when the record is in a certain state. For example: Nexus
/// should not undo a volume modification _after_ an upstairs has been sent a
/// replacement request, so volume modification happens at the Allocating state
/// (in the start saga), and replacement requests are only sent in the Driving
/// state (in the drive saga) - this ensures that replacement requests are only
/// sent if the start saga completed successfully, meaning the volume
/// modification was committed to the database and will not change or be
/// unwound.
///
/// It's also possible to transition from Running to ReplacementDone if a
/// "finish" notification is seen by the region replacement drive background
/// task. This check is done before invoking the region replacement drive saga.
///
/// If the volume whose region is being replaced is soft-deleted or
/// hard-deleted, then the replacement request will be transitioned along the
/// states to Complete while avoiding operations that are meant to operate on
/// that volume. If the volume is soft-deleted or hard-deleted while the
/// replacement request is in the "Requested" state, the replacement request
/// will transition straight to Complete, and no operations will be performed.
///
/// See also: RegionReplacementStep records
#[derive(
    Queryable,
    Insertable,
    Debug,
    Clone,
    Selectable,
    Serialize,
    Deserialize,
    PartialEq,
)]
#[diesel(table_name = region_replacement)]
pub struct RegionReplacement {
    pub id: Uuid,

    pub request_time: DateTime<Utc>,

    /// The region being replaced
    pub old_region_id: Uuid,

    /// The volume whose region is being replaced
    pub volume_id: DbTypedUuid<VolumeKind>,

    /// A synthetic volume that only is used to later delete the old region
    pub old_region_volume_id: Option<DbTypedUuid<VolumeKind>>,

    /// The new region that will be used to replace the old one
    pub new_region_id: Option<Uuid>,

    pub replacement_state: RegionReplacementState,

    pub operating_saga_id: Option<Uuid>,
}

impl RegionReplacement {
    pub fn for_region(region: &Region) -> Self {
        Self::new(region.id(), region.volume_id())
    }

    pub fn new(old_region_id: Uuid, volume_id: VolumeUuid) -> Self {
        Self {
            id: Uuid::new_v4(),
            request_time: Utc::now(),
            old_region_id,
            volume_id: volume_id.into(),
            old_region_volume_id: None,
            new_region_id: None,
            replacement_state: RegionReplacementState::Requested,
            operating_saga_id: None,
        }
    }

    pub fn volume_id(&self) -> VolumeUuid {
        self.volume_id.into()
    }

    pub fn old_region_volume_id(&self) -> Option<VolumeUuid> {
        self.old_region_volume_id.map(|u| u.into())
    }
}
