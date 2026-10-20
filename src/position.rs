//! A position in the game's world space, `#[repr(C)]` as it's shared with the overlay (and in the shared memory).

use nalgebra::Vector3;
use std::fmt;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Position {
    pub fn vector(&self) -> Vector3<f32> {
        Vector3::new(self.x, self.y, self.z)
    }

    pub fn distance(&self, other: &Position) -> f32 {
        (self.vector() - other.vector()).norm()
    }

    pub fn midpoint(&self, other: &Position) -> Position {
        Position::from((self.vector() + other.vector()) / 2.0)
    }
}

impl From<Vector3<f32>> for Position {
    fn from(v: Vector3<f32>) -> Self {
        Position { x: v.x, y: v.y, z: v.z }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x, self.y, self.z)
    }
}
