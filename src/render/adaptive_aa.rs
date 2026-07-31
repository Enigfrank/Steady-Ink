use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::ink::{ActiveInkPreview, InkSurfaceConfig, InkTool};

const FRAME_WINDOW_CAPACITY: usize = 30;
const MIN_FRAME_SAMPLES: usize = 8;
const LOWER_QUALITY_THRESHOLD: Duration = Duration::from_millis(16);
const RAISE_QUALITY_THRESHOLD: Duration = Duration::from_millis(10);
const ADJUSTMENT_COOLDOWN: Duration = Duration::from_secs(1);
const FINE_STROKE_MAX_WIDTH: f32 = 6.0;

/// 活动画笔预览使用的固定平衡档位；擦除预览另行使用 Off。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PreviewAaQuality {
    Msaa2,
    Supersample2,
}

impl PreviewAaQuality {
    /// 返回当前预览质量对应的精确 surface 配置。
    pub(super) const fn surface_config(self) -> InkSurfaceConfig {
        match self {
            Self::Msaa2 => InkSurfaceConfig::for_msaa_samples(2),
            Self::Supersample2 => {
                InkSurfaceConfig::for_mode(crate::settings::InkAntialiasingMode::Supersample)
            }
        }
    }

    /// 返回相邻的更低质量档位。
    const fn lower(self) -> Self {
        match self {
            Self::Supersample2 => Self::Msaa2,
            Self::Msaa2 => Self::Msaa2,
        }
    }

    /// 返回相邻的更高质量档位。
    const fn higher(self) -> Self {
        match self {
            Self::Msaa2 => Self::Supersample2,
            Self::Supersample2 => Self::Supersample2,
        }
    }
}

/// 为活动画笔预览在 MSAA 2x/超采样 2x 间选择质量性能平衡。
pub(super) struct AdaptiveAaPolicy {
    adaptive_limit: PreviewAaQuality,
    frame_times: VecDeque<Duration>,
    last_adjustment: Option<Instant>,
}

impl AdaptiveAaPolicy {
    /// 创建以超采样 2x 为上限的空帧时间窗口。
    pub(super) fn new() -> Self {
        Self {
            adaptive_limit: PreviewAaQuality::Supersample2,
            frame_times: VecDeque::with_capacity(FRAME_WINDOW_CAPACITY),
            last_adjustment: None,
        }
    }

    /// 清除跨模式的帧时间历史，并恢复超采样 2x 初始上限。
    pub(super) fn reset_runtime_state(&mut self) {
        self.adaptive_limit = PreviewAaQuality::Supersample2;
        self.frame_times.clear();
        self.last_adjustment = None;
    }

    /// 根据活动画笔最细宽度和当前帧时间上限选择 MSAA 2x 或超采样 2x。
    pub(super) fn preview_quality(
        &self,
        preview: ActiveInkPreview<'_>,
    ) -> Option<PreviewAaQuality> {
        match preview_profile(preview) {
            PreviewProfile::Eraser => None,
            PreviewProfile::FinePen => {
                Some(PreviewAaQuality::Supersample2.min(self.adaptive_limit))
            }
            PreviewProfile::BroadPen => Some(PreviewAaQuality::Msaa2),
        }
    }

    /// 记录一个活动画笔预览帧耗时，并按双阈值和冷却期调整采样上限。
    pub(super) fn record_preview_frame(&mut self, frame_time: Duration, now: Instant) -> bool {
        self.frame_times.push_back(frame_time);
        while self.frame_times.len() > FRAME_WINDOW_CAPACITY {
            self.frame_times.pop_front();
        }
        if self.frame_times.len() < MIN_FRAME_SAMPLES
            || self
                .last_adjustment
                .is_some_and(|last| now.saturating_duration_since(last) < ADJUSTMENT_COOLDOWN)
        {
            return false;
        }

        let average = average_duration(&self.frame_times);
        let next_limit = if average > LOWER_QUALITY_THRESHOLD {
            self.adaptive_limit.lower()
        } else if average < RAISE_QUALITY_THRESHOLD {
            self.adaptive_limit.higher()
        } else {
            self.adaptive_limit
        };
        if next_limit == self.adaptive_limit {
            return false;
        }

        self.adaptive_limit = next_limit;
        self.frame_times.clear();
        self.last_adjustment = Some(now);
        true
    }

    /// 返回当前自适应质量上限，供低频调试日志使用。
    pub(super) const fn adaptive_limit(&self) -> PreviewAaQuality {
        self.adaptive_limit
    }
}

/// 活动预览按擦除、细笔和粗笔划分的静态策略输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewProfile {
    Eraser,
    FinePen,
    BroadPen,
}

/// 从活动预览提取工具类别和最细可见宽度。
fn preview_profile(preview: ActiveInkPreview<'_>) -> PreviewProfile {
    let width = match preview {
        ActiveInkPreview::Tool {
            tool: InkTool::RegionEraser,
            ..
        }
        | ActiveInkPreview::PalmErase { .. } => return PreviewProfile::Eraser,
        ActiveInkPreview::Tool {
            tool: InkTool::Pen,
            pen_width,
            ..
        } => pen_width.pixels(),
        ActiveInkPreview::VariableTool { points, .. } => points
            .iter()
            .map(|sample| sample.width)
            .filter(|width| width.is_finite() && *width >= 0.0)
            .reduce(f32::min)
            .unwrap_or(0.0),
    };
    if width <= FINE_STROKE_MAX_WIDTH {
        PreviewProfile::FinePen
    } else {
        PreviewProfile::BroadPen
    }
}

/// 以纳秒整数平均一组耗时，避免浮点阈值漂移。
fn average_duration(samples: &VecDeque<Duration>) -> Duration {
    let average_nanos =
        samples.iter().map(Duration::as_nanos).sum::<u128>() / samples.len().max(1) as u128;
    Duration::from_nanos(average_nanos.min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::{CanvasPoint, EraserSize, InkColor, PenWidth, VariableStrokePoint};

    /// 创建固定宽度画笔预览。
    fn fixed_pen(width: PenWidth) -> ActiveInkPreview<'static> {
        static POINTS: [CanvasPoint; 1] = [CanvasPoint { x: 1.0, y: 1.0 }];
        ActiveInkPreview::Tool {
            points: &POINTS,
            tool: InkTool::Pen,
            color: InkColor::Red,
            pen_width: width,
            eraser_size: EraserSize::Px36,
        }
    }

    /// 验证 4px/6px 和细笔锋使用 2x 超采样，宽笔使用 MSAA 2x，橡皮擦使用 Off。
    #[test]
    fn fixed_balance_uses_tool_and_width() {
        let policy = AdaptiveAaPolicy::new();
        let high_quality_config = PreviewAaQuality::Supersample2.surface_config();
        let thin_points = [VariableStrokePoint {
            point: CanvasPoint::new(1.0, 1.0),
            width: 2.0,
        }];
        let thin = ActiveInkPreview::VariableTool {
            points: &thin_points,
            color: InkColor::Red,
            eraser_size: EraserSize::Px36,
        };
        let eraser = ActiveInkPreview::Tool {
            points: &[CanvasPoint::new(1.0, 1.0)],
            tool: InkTool::RegionEraser,
            color: InkColor::Red,
            pen_width: PenWidth::Px4,
            eraser_size: EraserSize::Px36,
        };

        assert_eq!(high_quality_config.render_scale, 2.0);
        assert!(high_quality_config.requires_linear_sampling());
        assert_eq!(
            policy.preview_quality(thin),
            Some(PreviewAaQuality::Supersample2)
        );
        assert_eq!(
            policy.preview_quality(fixed_pen(PenWidth::Px4)),
            Some(PreviewAaQuality::Supersample2)
        );
        assert_eq!(
            policy.preview_quality(fixed_pen(PenWidth::Px6)),
            Some(PreviewAaQuality::Supersample2)
        );
        assert_eq!(
            policy.preview_quality(fixed_pen(PenWidth::Px8)),
            Some(PreviewAaQuality::Msaa2)
        );
        assert_eq!(policy.preview_quality(eraser), None);
    }

    /// 验证慢帧降至 2x、冷却期阻止抖动且快帧恢复到 4x。
    #[test]
    fn frame_hysteresis_respects_cooldown() {
        let start = Instant::now();
        let mut policy = AdaptiveAaPolicy::new();
        for _ in 0..MIN_FRAME_SAMPLES {
            policy.record_preview_frame(Duration::from_millis(20), start);
        }
        assert_eq!(policy.adaptive_limit(), PreviewAaQuality::Msaa2);
        assert_eq!(
            policy.preview_quality(fixed_pen(PenWidth::Px4)),
            Some(PreviewAaQuality::Msaa2)
        );

        for _ in 0..MIN_FRAME_SAMPLES {
            policy
                .record_preview_frame(Duration::from_millis(5), start + Duration::from_millis(500));
        }
        assert_eq!(policy.adaptive_limit(), PreviewAaQuality::Msaa2);

        policy.record_preview_frame(Duration::from_millis(5), start + Duration::from_secs(1));
        assert_eq!(policy.adaptive_limit(), PreviewAaQuality::Supersample2);
    }
}
