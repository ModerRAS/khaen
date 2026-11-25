//! 鞋盒房间几何：Position / SurfaceReflections / RoomModel。
//!
//! 轴向约定（与 RoomForge 一致，写死一次）：x = Length，y = Width，z = Height；
//! 房间占 [0, Length] × [0, Width] × [0, Height]，单位 m。

/// A point in the room, in metres.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Position {
    pub const fn new(x: f64, y: f64, z: f64) -> Position {
        Position { x, y, z }
    }

    pub fn distance_to(self, other: Position) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    pub fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }
}

/// The six surfaces of the box, each with one frequency-independent pressure reflection coefficient in [0, 1].
/// 正系数 = 刚性（声压波腹）边界，与 RoomForge 相同的符号约定。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceReflections {
    pub left: f64,
    pub right: f64,
    pub front: f64,
    pub rear: f64,
    pub floor: f64,
    pub ceiling: f64,
}

impl SurfaceReflections {
    pub const fn new(
        left: f64,
        right: f64,
        front: f64,
        rear: f64,
        floor: f64,
        ceiling: f64,
    ) -> SurfaceReflections {
        SurfaceReflections {
            left,
            right,
            front,
            rear,
            floor,
            ceiling,
        }
    }

    /// RoomForge 默认：四面墙 0.7、地板/天花板 0.5。
    pub const DEFAULT: SurfaceReflections = SurfaceReflections::new(0.7, 0.7, 0.7, 0.7, 0.5, 0.5);

    pub fn validate(&self) -> Result<(), String> {
        let values = [
            ("left", self.left),
            ("right", self.right),
            ("front", self.front),
            ("rear", self.rear),
            ("floor", self.floor),
            ("ceiling", self.ceiling),
        ];
        for (name, value) in values {
            if !(value >= 0.0 && value <= 1.0) {
                return Err(format!(
                    "SurfaceReflections::{name} must be a reflection coefficient in [0, 1], got {value}"
                ));
            }
        }
        Ok(())
    }
}

impl Default for SurfaceReflections {
    fn default() -> SurfaceReflections {
        SurfaceReflections::DEFAULT
    }
}

/// A rectangular (shoebox) room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomModel {
    pub length_m: f64,
    pub width_m: f64,
    pub height_m: f64,
    pub speed_of_sound: f64,
    pub reflections: SurfaceReflections,
}

impl RoomModel {
    pub const fn new(
        length_m: f64,
        width_m: f64,
        height_m: f64,
        speed_of_sound: f64,
        reflections: SurfaceReflections,
    ) -> RoomModel {
        RoomModel {
            length_m,
            width_m,
            height_m,
            speed_of_sound,
            reflections,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(self.length_m > 0.0) {
            return Err(format!("length_m must be > 0, got {}", self.length_m));
        }
        if !(self.width_m > 0.0) {
            return Err(format!("width_m must be > 0, got {}", self.width_m));
        }
        if !(self.height_m > 0.0) {
            return Err(format!("height_m must be > 0, got {}", self.height_m));
        }
        if !(self.speed_of_sound > 0.0) {
            return Err(format!(
                "speed_of_sound must be > 0, got {}",
                self.speed_of_sound
            ));
        }
        self.reflections.validate()
    }

    pub fn volume_m3(&self) -> f64 {
        self.length_m * self.width_m * self.height_m
    }

    /// 六面表面积 (m²)：左/右 = W·H，前/后 = L·H，地/顶 = L·W。
    pub fn surface_areas(&self) -> [f64; 6] {
        let side = self.width_m * self.height_m;
        let end = self.length_m * self.height_m;
        let floor = self.length_m * self.width_m;
        [side, side, end, end, floor, floor]
    }

    /// Sabine T60 = 0.161·V / Σ(α_i·A_i)，α_i = 1 − r_i²（契约 §2）。
    pub fn sabine_t60_s(&self) -> f64 {
        let areas = self.surface_areas();
        let reflections = [
            self.reflections.left,
            self.reflections.right,
            self.reflections.front,
            self.reflections.rear,
            self.reflections.floor,
            self.reflections.ceiling,
        ];
        let absorption: f64 = reflections
            .iter()
            .zip(areas.iter())
            .map(|(r, a)| (1.0 - r * r) * a)
            .sum();
        0.161 * self.volume_m3() / absorption
    }
}

impl Default for RoomModel {
    fn default() -> RoomModel {
        RoomModel::new(3.3, 3.6, 2.6, 343.0, SurfaceReflections::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_distance() {
        let a = Position::new(0.0, 0.0, 0.0);
        let b = Position::new(3.0, 4.0, 12.0);
        assert!((a.distance_to(b) - 13.0).abs() < 1e-15);
        assert_eq!(b.to_array(), [3.0, 4.0, 12.0]);
    }

    #[test]
    fn defaults_match_roomforge() {
        let room = RoomModel::default();
        assert_eq!(
            room.reflections,
            SurfaceReflections::new(0.7, 0.7, 0.7, 0.7, 0.5, 0.5)
        );
        assert!(room.validate().is_ok());
        // 默认房间 3.3×3.6×2.6 + 默认反射：Sabine ≈ 0.137684 s。
        assert!((room.sabine_t60_s() - 0.1376836439748829).abs() < 1e-12);
        // 契约 §2 的 4.10×5.20×2.70 房间 + RoomForge 默认反射 ≈ 0.160921 s。
        let contract_room = RoomModel::new(4.10, 5.20, 2.70, 343.0, SurfaceReflections::DEFAULT);
        assert!((contract_room.sabine_t60_s() - 0.1609211664079511).abs() < 1e-12);
    }

    #[test]
    fn sabine_matches_contract_2x9_room() {
        let room = RoomModel::new(
            4.10,
            5.20,
            2.70,
            343.0,
            SurfaceReflections::new(0.866, 0.866, 0.866, 0.866, 0.866, 0.866),
        );
        // 契约 §2：≈0.3992 s（0.866 视作 sqrt(3)/2 时精确 0.3992；按 0.866 文本值 ≈0.39915）。
        let t60 = room.sabine_t60_s();
        assert!((t60 - 0.3992).abs() < 5e-4, "t60 = {t60}");
        assert!((room.volume_m3() - 57.564).abs() < 1e-9);
    }

    #[test]
    fn validate_rejects_out_of_range_reflections() {
        let bad = SurfaceReflections::new(1.2, 0.7, 0.7, 0.7, 0.5, 0.5);
        assert!(bad.validate().is_err());
        let room = RoomModel::new(4.1, 5.2, 2.7, 343.0, SurfaceReflections::DEFAULT);
        assert!(room.validate().is_ok());
    }
}
