//! The robot's nav map, as `NavMapFeed` delivers it.
//!
//! Vector's map is a planar quadtree of labelled cells, built from his
//! time-of-flight proximity sensor, his four cliff sensors and his wheel
//! odometry. It is not SLAM: his pose is dead reckoning, corrected only when he
//! sees his charger's marker, and the whole map is thrown away whenever he is
//! picked up. `docs/robot-api.md` records the engine source behind all of this.
//!
//! These are the types as they arrive, in domain terms, so that this crate
//! stays free of `wirepod-proto`. `wirepod-vector` maps the proto into them.

/// What a cell holds, as the SDK's `NavNodeContentType` numbers it.
///
/// Two pairs are worth knowing apart. [`NavContent::ClearOfObstacle`] and
/// [`NavContent::ClearOfCliff`] are independent, because the proximity sensor
/// and the cliff sensors write separately, and only driving over a cell
/// produces the second. [`NavContent::ObstacleProximity`] and
/// [`NavContent::ObstacleProximityExplored`] are the difference between
/// something the sensor hit and something he has since turned and looked at.
/// [`NavContent::InterestingEdge`] exists in the enum but this firmware never
/// writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NavContent {
    Unknown,
    ClearOfObstacle,
    ClearOfCliff,
    ObstacleCube,
    ObstacleProximity,
    ObstacleProximityExplored,
    ObstacleUnrecognized,
    Cliff,
    InterestingEdge,
    NonInterestingEdge,
}

impl NavContent {
    /// Every value, in wire order.
    pub const ALL: [Self; 10] = [
        Self::Unknown,
        Self::ClearOfObstacle,
        Self::ClearOfCliff,
        Self::ObstacleCube,
        Self::ObstacleProximity,
        Self::ObstacleProximityExplored,
        Self::ObstacleUnrecognized,
        Self::Cliff,
        Self::InterestingEdge,
        Self::NonInterestingEdge,
    ];

    /// The content a wire number names, or `None` for a number this build
    /// does not know.
    pub fn from_wire(value: i32) -> Option<Self> {
        usize::try_from(value)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
    }

    /// The wire number.
    pub fn wire(self) -> i32 {
        self as i32
    }

    /// The snake_case name the map page's `counts` object uses as a key.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::ClearOfObstacle => "clear_of_obstacle",
            Self::ClearOfCliff => "clear_of_cliff",
            Self::ObstacleCube => "obstacle_cube",
            Self::ObstacleProximity => "obstacle_proximity",
            Self::ObstacleProximityExplored => "obstacle_proximity_explored",
            Self::ObstacleUnrecognized => "obstacle_unrecognized",
            Self::Cliff => "cliff",
            Self::InterestingEdge => "interesting_edge",
            Self::NonInterestingEdge => "non_interesting_edge",
        }
    }
}

/// The map as a whole: its root square.
///
/// Millimetres, in the coordinate frame the frame's `origin_id` names. The
/// proto also carries a `root_center_z`, which the robot's gateway always sets
/// to zero, so it is not kept.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavMapInfo {
    /// The root's remaining height: the number of halvings from the root down
    /// to the finest possible leaf.
    pub root_depth: i32,
    pub root_size_mm: f32,
    pub root_center_x: f32,
    pub root_center_y: f32,
}

/// One leaf, exactly as it arrives. It carries no position: that is implied by
/// its place in the list and its `depth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavMapQuad {
    /// The `NavNodeContentType` number, kept raw so an unknown one survives;
    /// [`NavContent::from_wire`] names it.
    pub content: i32,
    /// **Remaining height, not depth from the root.** The root carries
    /// `root_depth`, each level down is one less, and a finest leaf carries 0.
    pub depth: u32,
    /// The robot's own render colour, packed red in the high byte and alpha in
    /// the low byte. It encodes a proximity obstacle's belief and a cliff's
    /// provenance, neither of which is otherwise on the wire.
    pub rgba: u32,
}

/// One `NavMapFeedResponse`.
#[derive(Clone, Debug, PartialEq)]
pub struct NavMapFrame {
    /// The coordinate frame the map is in. A pose with a different origin
    /// cannot be placed on this map at all.
    pub origin_id: u32,
    pub info: NavMapInfo,
    /// The leaves, in the robot's pre-order.
    pub quads: Vec<NavMapQuad>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_numbers_round_trip_and_an_unknown_one_is_refused() {
        for (index, content) in NavContent::ALL.iter().enumerate() {
            assert_eq!(content.wire(), index as i32);
            assert_eq!(NavContent::from_wire(index as i32), Some(*content));
        }
        assert_eq!(NavContent::from_wire(10), None);
        assert_eq!(NavContent::from_wire(-1), None);
        assert_eq!(
            NavContent::ObstacleProximityExplored.name(),
            "obstacle_proximity_explored"
        );
    }
}
