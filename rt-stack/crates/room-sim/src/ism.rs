//! 鞋盒镜像源法（Allen–Berkley），逐位移植 RoomForge AudioOptimizer.Simulation ImageSourceMethod.cs。
//!
//! 每个镜像源 (i, j, k) 是各轴镜像副本的组合；每轴坐标：
//!   coord(n) = (n 偶) ? n·size + s : (n + 1)·size − s
//! 一次反射命中乘对应墙面反射系数（按路径，不做事后 EQ）；幅度 = 反射幂次积 × refDist / 距离；
//! 延迟 = 距离 / 声速。固定 (i, j, k) 顺序输出（i 外层、k 内层，与 C# 一致），调用方求和所以顺序只影响
//! 浮点舍入、不影响物理结果。

use crate::room::{Position, RoomModel};

/// 一条传播路径：到达时间（s）与声压幅度（源 1 m 处幅度 1 的标定）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrival {
    pub delay_seconds: f64,
    pub amplitude: f64,
}

impl Arrival {
    pub fn new(delay_seconds: f64, amplitude: f64) -> Arrival {
        Arrival {
            delay_seconds,
            amplitude,
        }
    }
}

/// 一条 arrival 写成 IR 样本的方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalPlacement {
    /// 四舍五入到最近整数样本（half away from zero，与 C# MidpointRounding.AwayFromZero 一致）。
    NearestSample,
    /// 整数延迟 lower = floor(pos)，lower 与 lower+1 按小数部分分摊（v1 默认）。
    LinearInterpolated,
}

impl ArrivalPlacement {
    /// ground-truth.json 的 image_source.placement 字符串。
    pub fn from_ground_truth_str(value: &str) -> Option<ArrivalPlacement> {
        match value {
            "nearest_sample" => Some(ArrivalPlacement::NearestSample),
            "linear_interpolated" => Some(ArrivalPlacement::LinearInterpolated),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ArrivalPlacement::NearestSample => "nearest_sample",
            ArrivalPlacement::LinearInterpolated => "linear_interpolated",
        }
    }
}

/// 路径距离小于此值时 1/r 无定义；C# 跳过这种「源正好落在镜像副本上」的巧合路径。
pub const MIN_PATH_DISTANCE_M: f64 = 1e-12;

/// 镜像源 index 沿一轴的坐标（C# Coordinate 的逐式移植）。
pub fn coordinate(index: i64, size_m: f64, coordinate_m: f64) -> f64 {
    if index % 2 == 0 {
        (index as f64) * size_m + coordinate_m
    } else {
        ((index + 1) as f64) * size_m - coordinate_m
    }
}

/// 镜像源 index 在低墙（坐标 0）与高墙（坐标 size）上的命中次数（C# WallHits 的逐式移植）。
pub fn wall_hits(index: i64) -> (u32, u32) {
    let crossings = index.unsigned_abs() as u32;
    let half = crossings / 2;
    if index >= 0 {
        (half, crossings - half)
    } else {
        (crossings - half, half)
    }
}

/// 每轴 ±order 阶的全部镜像源路径，顺序固定 (i 外层, j, k 内层)，与 C# ImageSourceMethod.Arrivals 一致。
///
/// # Panics
/// 房间参数非法、reference_distance_m 非正或非有限时 panic（对应 C# 的 ArgumentException）。
pub fn arrivals(
    room: &RoomModel,
    source: Position,
    microphone: Position,
    order: u32,
    reference_distance_m: f64,
) -> Vec<Arrival> {
    if let Err(error) = room.validate() {
        panic!("ISM: invalid room: {error}");
    }
    assert!(
        reference_distance_m > 0.0 && reference_distance_m.is_finite(),
        "ISM: reference distance must be > 0 m and finite, got {reference_distance_m}"
    );

    let order = order as i64;
    let reflections = room.reflections;
    let side = (2 * order + 1) as usize;
    let mut result = Vec::with_capacity(side * side * side);

    for i in -order..=order {
        let image_x = coordinate(i, room.length_m, source.x);
        let (x_low, x_high) = wall_hits(i);
        let reflection_x =
            reflections.left.powf(x_low as f64) * reflections.right.powf(x_high as f64);

        for j in -order..=order {
            let image_y = coordinate(j, room.width_m, source.y);
            let (y_low, y_high) = wall_hits(j);
            let reflection_y = reflection_x
                * reflections.front.powf(y_low as f64)
                * reflections.rear.powf(y_high as f64);

            for k in -order..=order {
                let image_z = coordinate(k, room.height_m, source.z);
                let (z_low, z_high) = wall_hits(k);
                let reflection = reflection_y
                    * reflections.floor.powf(z_low as f64)
                    * reflections.ceiling.powf(z_high as f64);

                let dx = image_x - microphone.x;
                let dy = image_y - microphone.y;
                let dz = image_z - microphone.z;
                let distance = (dx * dx + dy * dy + dz * dz).sqrt();

                // 接收者正好落在镜像副本上：1/r 无定义，跳过而不是硬塞一个值（与 C# 一致）。
                if distance < MIN_PATH_DISTANCE_M {
                    continue;
                }

                result.push(Arrival::new(
                    distance / room.speed_of_sound,
                    reflection * reference_distance_m / distance,
                ));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::SurfaceReflections;

    fn hand_room() -> RoomModel {
        // 2 × 2 × 2 m，非对称反射便于逐面验证幂次。
        RoomModel::new(
            2.0,
            2.0,
            2.0,
            343.0,
            SurfaceReflections::new(0.5, 0.6, 0.7, 0.8, 0.9, 0.4),
        )
    }

    #[test]
    fn coordinate_and_wall_hits_match_csharp() {
        // 偶 index 在源坐标加整数倍尺寸；奇 index 反射到 (index+1)·size − s。
        assert_eq!(coordinate(0, 2.0, 1.0), 1.0);
        assert_eq!(coordinate(1, 2.0, 1.0), 3.0);
        assert_eq!(coordinate(-1, 2.0, 1.0), -1.0);
        assert_eq!(coordinate(2, 2.0, 1.0), 5.0);
        assert_eq!(coordinate(-2, 2.0, 1.0), -3.0);

        assert_eq!(wall_hits(0), (0, 0));
        assert_eq!(wall_hits(1), (0, 1));
        assert_eq!(wall_hits(-1), (1, 0));
        assert_eq!(wall_hits(2), (1, 1));
        assert_eq!(wall_hits(-2), (1, 1));
        assert_eq!(wall_hits(3), (1, 2));
        assert_eq!(wall_hits(-3), (2, 1));
        assert_eq!(wall_hits(12), (6, 6));
        assert_eq!(wall_hits(-12), (6, 6));
    }

    #[test]
    fn order_zero_is_the_direct_sound_only() {
        let room = hand_room();
        let source = Position::new(1.0, 1.0, 1.0);
        let microphone = Position::new(1.5, 1.0, 1.0);
        let direct = arrivals(&room, source, microphone, 0, 1.0);
        assert_eq!(direct.len(), 1);
        assert!((direct[0].delay_seconds - 0.5 / 343.0).abs() < 1e-15);
        assert!((direct[0].amplitude - 2.0).abs() < 1e-15);
    }

    #[test]
    fn order_one_hand_checked_arrivals() {
        let room = hand_room();
        let source = Position::new(1.0, 1.0, 1.0);
        let microphone = Position::new(1.5, 1.0, 1.0);
        let paths = arrivals(&room, source, microphone, 1, 1.0);
        assert_eq!(paths.len(), 27);

        // 每轴 3 个 index，9 条 (j, k) 路径一块：i=-1 块 0..8，i=0 块 9..17，i=+1 块 18..26。
        // i=-1, j=0, k=0：命中左墙一次（left = 0.5），距离 2.5 m。
        let left = paths[4];
        assert!((left.delay_seconds - 2.5 / 343.0).abs() < 1e-15);
        assert!((left.amplitude - 0.5 / 2.5).abs() < 1e-15);
        // i=+1, j=0, k=0：命中右墙一次（right = 0.6），距离 1.5 m。
        let right = paths[22];
        assert!((right.delay_seconds - 1.5 / 343.0).abs() < 1e-15);
        assert!((right.amplitude - 0.6 / 1.5).abs() < 1e-15);
        // i=0, j=-1, k=0：命中前墙（front = 0.7），距离 sqrt(0.5² + 2.0²) m。
        let front = paths[10];
        let front_distance = (0.25f64 + 4.0).sqrt();
        assert!((front.delay_seconds - front_distance / 343.0).abs() < 1e-15);
        assert!((front.amplitude - 0.7 / front_distance).abs() < 1e-15);
        // i=0, j=0, k=-1：命中地板（floor = 0.9），距离 sqrt(0.5² + 2.0²) m。
        let floor = paths[12];
        let floor_distance = (0.25f64 + 4.0).sqrt();
        assert!((floor.delay_seconds - floor_distance / 343.0).abs() < 1e-15);
        assert!((floor.amplitude - 0.9 / floor_distance).abs() < 1e-15);
        // i=-1, j=-1, k=-1：left·front·floor，距离 sqrt(2.5² + 2.0² + 2.0²)。
        let corner = paths[0];
        let expected_distance = (6.25f64 + 4.0 + 4.0).sqrt();
        assert!((corner.delay_seconds - expected_distance / 343.0).abs() < 1e-15);
        assert!((corner.amplitude - 0.5 * 0.7 * 0.9 / expected_distance).abs() < 1e-15);
    }

    #[test]
    fn higher_order_count_and_reference_distance_scaling() {
        let room = hand_room();
        let source = Position::new(0.31, 1.27, 0.83);
        let microphone = Position::new(1.44, 0.62, 1.51);
        let paths = arrivals(&room, source, microphone, 3, 2.0);
        assert_eq!(paths.len(), 7 * 7 * 7);
        let unit = arrivals(&room, source, microphone, 3, 1.0);
        assert_eq!(unit.len(), paths.len());
        for (scaled, base) in paths.iter().zip(unit.iter()) {
            assert!((scaled.delay_seconds - base.delay_seconds).abs() < 1e-15);
            assert!((scaled.amplitude - 2.0 * base.amplitude).abs() < 1e-15);
        }
    }

    #[test]
    fn coincident_receiver_is_skipped() {
        let room = hand_room();
        let source = Position::new(0.5, 0.5, 0.5);
        assert!(arrivals(&room, source, source, 0, 1.0).is_empty());
        // 0 阶没有其他路径；高墙镜像 (2·2 − 0.5 = 3.5) 不会与源重合。
        assert_eq!(arrivals(&room, source, source, 1, 1.0).len(), 26);
    }
}
