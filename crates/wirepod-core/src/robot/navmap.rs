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
//! [`reconstruct`] turns a frame's list of leaves back into placed squares.

use std::fmt;

/// What a cell holds, as the SDK's `NavNodeContentType` numbers it.
///
/// Two pairs are worth knowing apart. [`NavContent::ClearOfObstacle`] and
/// [`NavContent::ClearOfCliff`] are independent, because the proximity sensor
/// and the cliff sensors write separately. The second comes from driving over
/// a cell, and from going home, which marks the charger's docking area clear
/// of cliffs before he drives onto it. [`NavContent::ObstacleProximity`] and
/// [`NavContent::ObstacleProximityExplored`] are the difference between
/// something the sensor hit and something he has since turned and looked at.
/// [`NavContent::InterestingEdge`] and [`NavContent::NonInterestingEdge`] exist
/// in the enum but a stock robot writes neither: the first has no writer, and
/// the only writer of the second has no callers.
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
    /// the low byte. It encodes a cliff's provenance, which is not otherwise on
    /// the wire. It would also shade a proximity obstacle by the robot's belief
    /// in it, but the engine does that only while its `kRenderProxBeliefs`
    /// switch is on, and that defaults off.
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

/// A leaf placed on the plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedQuad {
    /// The centre in millimetres, in the frame's `origin_id` frame.
    pub cx: f32,
    pub cy: f32,
    /// The side in millimetres.
    pub side: f32,
    /// The `NavNodeContentType` number, as it arrived.
    pub content: i32,
    /// The robot's packed colour, as it arrived.
    pub rgba: u32,
}

/// The deepest root [`reconstruct`] will place.
///
/// The robot's own root stops at depth 8, a two-metre square of 8 mm leaves.
/// Every level of a leaf's descent is a frame on the stack, so without a bound
/// a corrupt `root_depth` near `i32::MAX` would exhaust memory before the list
/// ran out.
pub const MAX_ROOT_DEPTH: u32 = 32;

/// Why a frame's leaves do not tile its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconstructError {
    /// The root claims more levels than [`MAX_ROOT_DEPTH`].
    RootTooDeep { root_depth: i32 },
    /// A leaf claims more height than the root has. Under a negative
    /// `root_depth` every leaf does.
    DeeperThanRoot {
        index: usize,
        depth: u32,
        root_depth: i32,
    },
    /// A leaf claims more height than the free cell it lands in.
    Misplaced {
        index: usize,
        depth: u32,
        level: u32,
    },
    /// The list ended before the root was covered.
    RanOut { quads: usize },
    /// Leaves remained after the root was covered.
    LeftOver { used: usize, total: usize },
}

impl fmt::Display for ReconstructError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootTooDeep { root_depth } => write!(
                f,
                "malformed nav map: root depth {root_depth} is more than {MAX_ROOT_DEPTH}"
            ),
            Self::DeeperThanRoot {
                index,
                depth,
                root_depth,
            } => write!(
                f,
                "malformed nav map: quad {index} has depth {depth} under a root of depth {root_depth}"
            ),
            Self::Misplaced {
                index,
                depth,
                level,
            } => write!(
                f,
                "malformed nav map: quad {index} has depth {depth} where the next free cell has depth {level}"
            ),
            Self::RanOut { quads } => {
                write!(f, "malformed nav map: {quads} quads do not cover the root")
            }
            Self::LeftOver { used, total } => write!(
                f,
                "malformed nav map: the root was covered after {used} of {total} quads"
            ),
        }
    }
}

impl std::error::Error for ReconstructError {}

/// One square of the tree while [`reconstruct`] walks it.
#[derive(Clone, Copy, Debug)]
struct Cell {
    cx: f32,
    cy: f32,
    side: f32,
    level: u32,
    /// Which child is being filled, once this cell has been split.
    next_child: u8,
}

impl Cell {
    /// Child `index`, in the robot's fixed order: +x+y, +x−y, −x+y, −x−y. The
    /// offset is a quarter of this cell's side, which is the middle of each
    /// quadrant.
    fn child(&self, index: u8) -> Self {
        let off = self.side / 4.0;
        let (dx, dy) = match index {
            0 => (off, off),
            1 => (off, -off),
            2 => (-off, off),
            _ => (-off, -off),
        };
        Self {
            cx: self.cx + dx,
            cy: self.cy + dy,
            side: self.side / 2.0,
            level: self.level - 1,
            next_child: 0,
        }
    }
}

/// Places every leaf of `frame`, in the order the robot sent them.
///
/// A leaf's `depth` is its **remaining height**, not its distance from the
/// root: the root has `root_depth`, each level down is one less, and a finest
/// leaf has 0. Read the other way round, every position comes out wrong.
/// Nothing on the wire says where a leaf is; each one fills the next free cell,
/// which is split until its level equals the leaf's depth. This is the Python
/// SDK's `NavMapGridNode.add_child`, with a stack in place of its recursion.
pub fn reconstruct(frame: &NavMapFrame) -> Result<Vec<PlacedQuad>, ReconstructError> {
    let info = frame.info;
    let total = frame.quads.len();
    if let Some((index, quad)) = frame
        .quads
        .iter()
        .enumerate()
        .find(|(_, quad)| i64::from(quad.depth) > i64::from(info.root_depth))
    {
        return Err(ReconstructError::DeeperThanRoot {
            index,
            depth: quad.depth,
            root_depth: info.root_depth,
        });
    }
    let root_level = match u32::try_from(info.root_depth) {
        Ok(level) if level <= MAX_ROOT_DEPTH => level,
        Ok(_) => {
            return Err(ReconstructError::RootTooDeep {
                root_depth: info.root_depth,
            });
        }
        // Negative, with no leaves to be too deep for it.
        Err(_) => return Err(ReconstructError::RanOut { quads: 0 }),
    };

    // The cells split so far on the path down from the root, with the next
    // free cell on top.
    let mut stack = vec![Cell {
        cx: info.root_center_x,
        cy: info.root_center_y,
        side: info.root_size_mm,
        level: root_level,
        next_child: 0,
    }];
    let mut placed = Vec::with_capacity(total);
    for (index, quad) in frame.quads.iter().enumerate() {
        let Some(mut cell) = stack.pop() else {
            return Err(ReconstructError::LeftOver { used: index, total });
        };
        while quad.depth < cell.level {
            let first = cell.child(0);
            stack.push(cell);
            cell = first;
        }
        if quad.depth > cell.level {
            return Err(ReconstructError::Misplaced {
                index,
                depth: quad.depth,
                level: cell.level,
            });
        }
        placed.push(PlacedQuad {
            cx: cell.cx,
            cy: cell.cy,
            side: cell.side,
            content: quad.content,
            rgba: quad.rgba,
        });

        // The cell is full. Move on to its next sibling, closing every split
        // cell whose four children are now full.
        while let Some(parent) = stack.last_mut() {
            parent.next_child += 1;
            if parent.next_child < 4 {
                let sibling = parent.child(parent.next_child);
                stack.push(sibling);
                break;
            }
            stack.pop();
        }
    }
    if !stack.is_empty() {
        return Err(ReconstructError::RanOut { quads: total });
    }
    Ok(placed)
}

/// How many leaves carry each content type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentCounts {
    counts: [usize; NavContent::ALL.len()],
    unrecognised: usize,
}

impl ContentCounts {
    /// Counts `quads` by content.
    pub fn of(quads: &[NavMapQuad]) -> Self {
        let mut counts = Self::default();
        for quad in quads {
            match NavContent::from_wire(quad.content) {
                Some(content) => counts.counts[content as usize] += 1,
                None => counts.unrecognised += 1,
            }
        }
        counts
    }

    /// How many leaves carry `content`.
    pub fn get(&self, content: NavContent) -> usize {
        self.counts[content as usize]
    }

    /// How many leaves carry a content number this build does not know.
    pub fn unrecognised(&self) -> usize {
        self.unrecognised
    }

    /// Every content type and its count, in wire order.
    pub fn iter(&self) -> impl Iterator<Item = (NavContent, usize)> + '_ {
        NavContent::ALL
            .iter()
            .map(|content| (*content, self.get(*content)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(root_depth: i32, size: f32, cx: f32, cy: f32, depths: &[u32]) -> NavMapFrame {
        NavMapFrame {
            origin_id: 1,
            info: NavMapInfo {
                root_depth,
                root_size_mm: size,
                root_center_x: cx,
                root_center_y: cy,
            },
            quads: depths
                .iter()
                .enumerate()
                .map(|(index, depth)| NavMapQuad {
                    content: index as i32 % 10,
                    depth: *depth,
                    rgba: 0x1000 + index as u32,
                })
                .collect(),
        }
    }

    /// The centre and side of every placed leaf.
    fn squares(placed: &[PlacedQuad]) -> Vec<(f32, f32, f32)> {
        placed
            .iter()
            .map(|quad| (quad.cx, quad.cy, quad.side))
            .collect()
    }

    #[test]
    fn a_single_leaf_is_the_root() {
        let placed = reconstruct(&frame(3, 128.0, 64.0, 0.0, &[3])).expect("one leaf");
        assert_eq!(
            placed,
            vec![PlacedQuad {
                cx: 64.0,
                cy: 0.0,
                side: 128.0,
                content: 0,
                rgba: 0x1000,
            }]
        );
    }

    #[test]
    fn a_root_split_once_places_its_children_in_the_robots_order() {
        let placed = reconstruct(&frame(1, 100.0, 0.0, 0.0, &[0, 0, 0, 0])).expect("four leaves");
        assert_eq!(
            squares(&placed),
            vec![
                (25.0, 25.0, 50.0),
                (25.0, -25.0, 50.0),
                (-25.0, 25.0, 50.0),
                (-25.0, -25.0, 50.0),
            ]
        );
        // Content and colour stay with the leaf they arrived on.
        let contents: Vec<(i32, u32)> = placed.iter().map(|q| (q.content, q.rgba)).collect();
        assert_eq!(
            contents,
            vec![(0, 0x1000), (1, 0x1001), (2, 0x1002), (3, 0x1003)]
        );
    }

    #[test]
    fn a_mixed_depth_tree_places_every_leaf() {
        // Child 0 split again, the other three leaves.
        let placed = reconstruct(&frame(2, 400.0, 10.0, 20.0, &[0, 0, 0, 0, 1, 1, 1]))
            .expect("seven leaves");
        assert_eq!(
            squares(&placed),
            vec![
                (160.0, 170.0, 100.0),
                (160.0, 70.0, 100.0),
                (60.0, 170.0, 100.0),
                (60.0, 70.0, 100.0),
                (110.0, -80.0, 200.0),
                (-90.0, 120.0, 200.0),
                (-90.0, -80.0, 200.0),
            ]
        );

        // The last child split, so the root closes only after its grandchildren.
        let placed =
            reconstruct(&frame(2, 400.0, 0.0, 0.0, &[1, 1, 1, 0, 0, 0, 0])).expect("seven leaves");
        assert_eq!(
            squares(&placed),
            vec![
                (100.0, 100.0, 200.0),
                (100.0, -100.0, 200.0),
                (-100.0, 100.0, 200.0),
                (-50.0, -50.0, 100.0),
                (-50.0, -150.0, 100.0),
                (-150.0, -50.0, 100.0),
                (-150.0, -150.0, 100.0),
            ]
        );
    }

    #[test]
    fn a_malformed_list_is_an_error() {
        assert_eq!(
            reconstruct(&frame(1, 64.0, 0.0, 0.0, &[0, 2])),
            Err(ReconstructError::DeeperThanRoot {
                index: 1,
                depth: 2,
                root_depth: 1,
            })
        );
        assert_eq!(
            reconstruct(&frame(-1, 64.0, 0.0, 0.0, &[0])),
            Err(ReconstructError::DeeperThanRoot {
                index: 0,
                depth: 0,
                root_depth: -1,
            })
        );
        assert_eq!(
            reconstruct(&frame(-1, 64.0, 0.0, 0.0, &[])),
            Err(ReconstructError::RanOut { quads: 0 })
        );
        assert_eq!(
            reconstruct(&frame(1, 64.0, 0.0, 0.0, &[0, 0, 0])),
            Err(ReconstructError::RanOut { quads: 3 })
        );
        assert_eq!(
            reconstruct(&frame(0, 64.0, 0.0, 0.0, &[0, 0])),
            Err(ReconstructError::LeftOver { used: 1, total: 2 })
        );
        // A leaf as tall as the root, after one that already split it.
        assert_eq!(
            reconstruct(&frame(2, 64.0, 0.0, 0.0, &[0, 2])),
            Err(ReconstructError::Misplaced {
                index: 1,
                depth: 2,
                level: 0,
            })
        );
        assert_eq!(
            reconstruct(&frame(i32::MAX, 64.0, 0.0, 0.0, &[0])),
            Err(ReconstructError::RootTooDeep {
                root_depth: i32::MAX,
            })
        );
    }

    #[test]
    fn the_histogram_counts_leaves_by_content() {
        let quads: Vec<NavMapQuad> = [1, 1, 4, 7, 12]
            .into_iter()
            .map(|content| NavMapQuad {
                content,
                depth: 0,
                rgba: 0,
            })
            .collect();
        let counts = ContentCounts::of(&quads);
        assert_eq!(counts.get(NavContent::ClearOfObstacle), 2);
        assert_eq!(counts.get(NavContent::ObstacleProximity), 1);
        assert_eq!(counts.get(NavContent::Cliff), 1);
        assert_eq!(counts.get(NavContent::Unknown), 0);
        assert_eq!(counts.unrecognised(), 1);
        let listed: Vec<(NavContent, usize)> = counts.iter().filter(|(_, n)| *n > 0).collect();
        assert_eq!(
            listed,
            vec![
                (NavContent::ClearOfObstacle, 2),
                (NavContent::ObstacleProximity, 1),
                (NavContent::Cliff, 1),
            ]
        );
    }

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
