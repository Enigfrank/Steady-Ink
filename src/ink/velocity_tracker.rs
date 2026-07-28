use std::{collections::VecDeque, time::Instant};

use super::CanvasPoint;

const DEFAULT_SAMPLE_CAPACITY: usize = 5;
pub(crate) const BASE_PREVIEW_TILE_SIZE: u32 = 512;
pub(crate) const MEDIUM_PREVIEW_TILE_SIZE: u32 = 640;
pub(crate) const LARGE_PREVIEW_TILE_SIZE: u32 = 768;

/// 跟踪最近若干个不同位置采样并计算物理像素速度。
pub(crate) struct VelocityTracker {
    recent_samples: VecDeque<(CanvasPoint, Instant)>,
    max_samples: usize,
    velocity: f32,
}

impl VelocityTracker {
    /// 创建使用五个最近采样的速度追踪器。
    pub(crate) fn new() -> Self {
        Self {
            recent_samples: VecDeque::with_capacity(DEFAULT_SAMPLE_CAPACITY),
            max_samples: DEFAULT_SAMPLE_CAPACITY,
            velocity: 0.0,
        }
    }

    /// 添加一个不同位置的新采样并更新平均速度。
    pub(crate) fn update(&mut self, point: CanvasPoint, time: Instant) {
        if self
            .recent_samples
            .back()
            .is_some_and(|(previous, _)| *previous == point)
        {
            return;
        }
        self.recent_samples.push_back((point, time));
        while self.recent_samples.len() > self.max_samples {
            self.recent_samples.pop_front();
        }
        self.recalculate_velocity();
    }

    /// 返回当前平均物理像素速度。
    pub(crate) const fn velocity(&self) -> f32 {
        self.velocity
    }

    /// 清空手势采样和速度状态。
    pub(crate) fn reset(&mut self) {
        self.recent_samples.clear();
        self.velocity = 0.0;
    }

    /// 根据相邻采样的总距离和有效时间计算速度。
    fn recalculate_velocity(&mut self) {
        let mut total_distance = 0.0;
        let mut total_seconds = 0.0;
        for ((previous_point, previous_time), (next_point, next_time)) in self
            .recent_samples
            .iter()
            .zip(self.recent_samples.iter().skip(1))
        {
            let seconds = next_time
                .saturating_duration_since(*previous_time)
                .as_secs_f32();
            if seconds <= f32::EPSILON {
                continue;
            }
            let delta_x = next_point.x - previous_point.x;
            let delta_y = next_point.y - previous_point.y;
            total_distance += delta_x.mul_add(delta_x, delta_y * delta_y).sqrt();
            total_seconds += seconds;
        }
        self.velocity = if total_seconds > 0.0 {
            total_distance / total_seconds
        } else {
            0.0
        };
    }
}

impl Default for VelocityTracker {
    /// 创建默认速度追踪器。
    fn default() -> Self {
        Self::new()
    }
}

/// 把物理像素速度映射为自适应预览分块尺寸。
pub(crate) fn preview_tile_size_for_velocity(velocity: f32) -> u32 {
    if !velocity.is_finite() || velocity < 200.0 {
        BASE_PREVIEW_TILE_SIZE
    } else if velocity < 800.0 {
        MEDIUM_PREVIEW_TILE_SIZE
    } else {
        LARGE_PREVIEW_TILE_SIZE
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 验证已知距离和时间得到稳定的平均速度。
    #[test]
    fn velocity_uses_recent_distance_and_time() {
        let start = Instant::now();
        let mut tracker = VelocityTracker::new();
        tracker.update(CanvasPoint::new(0.0, 0.0), start);
        tracker.update(
            CanvasPoint::new(30.0, 40.0),
            start + Duration::from_millis(100),
        );

        assert!((tracker.velocity() - 500.0).abs() < 0.01);
    }

    /// 验证重复位置不会用零距离帧稀释活动手势速度。
    #[test]
    fn repeated_position_is_ignored() {
        let start = Instant::now();
        let mut tracker = VelocityTracker::new();
        tracker.update(CanvasPoint::new(0.0, 0.0), start);
        tracker.update(
            CanvasPoint::new(10.0, 0.0),
            start + Duration::from_millis(10),
        );
        let velocity = tracker.velocity();
        tracker.update(CanvasPoint::new(10.0, 0.0), start + Duration::from_secs(1));

        assert_eq!(tracker.velocity(), velocity);
    }

    /// 验证三档速度阈值和非法输入回退基础尺寸。
    #[test]
    fn velocity_maps_to_three_preview_sizes() {
        assert_eq!(preview_tile_size_for_velocity(f32::NAN), 512);
        assert_eq!(preview_tile_size_for_velocity(199.0), 512);
        assert_eq!(preview_tile_size_for_velocity(200.0), 640);
        assert_eq!(preview_tile_size_for_velocity(799.0), 640);
        assert_eq!(preview_tile_size_for_velocity(800.0), 768);
    }

    /// 验证重置会清空速度与历史采样。
    #[test]
    fn reset_clears_velocity() {
        let start = Instant::now();
        let mut tracker = VelocityTracker::new();
        tracker.update(CanvasPoint::new(0.0, 0.0), start);
        tracker.update(
            CanvasPoint::new(10.0, 0.0),
            start + Duration::from_millis(10),
        );

        tracker.reset();

        assert_eq!(tracker.velocity(), 0.0);
        assert!(tracker.recent_samples.is_empty());
    }
}
