use std::time::{Duration, Instant};

use skia_safe::{Color, Surface};

use super::InkSurfaceConfig;
use crate::error::AppError;

const BYTES_PER_PIXEL: usize = 4;

/// 可复用的离屏 Skia surface 资源池。
pub(crate) struct SurfacePool {
    idle_surfaces: Vec<PooledSurface>,
    max_entries: usize,
    max_estimated_bytes: usize,
    estimated_bytes: usize,
    reused_count: u64,
    created_count: u64,
    eviction_count: u64,
}

/// 一项带有精确配置和最近使用时间的闲置 surface。
struct PooledSurface {
    surface: Surface,
    render_size: [u32; 2],
    config: InkSurfaceConfig,
    estimated_bytes: usize,
    last_used: Instant,
}

/// Surface 资源池的只读诊断统计。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SurfacePoolStats {
    pub idle_count: usize,
    pub estimated_bytes: usize,
    pub reused_count: u64,
    pub created_count: u64,
    pub eviction_count: u64,
}

impl SurfacePoolStats {
    /// 返回自资源池创建以来的精确匹配命中率。
    pub(crate) fn hit_rate(self) -> f64 {
        let total = self.reused_count + self.created_count;
        if total == 0 {
            0.0
        } else {
            self.reused_count as f64 / total as f64
        }
    }
}

impl SurfacePool {
    /// 创建具有条目数和估算字节数双重限制的空池。
    pub(crate) const fn new(max_entries: usize, max_estimated_bytes: usize) -> Self {
        Self {
            idle_surfaces: Vec::new(),
            max_entries,
            max_estimated_bytes,
            estimated_bytes: 0,
            reused_count: 0,
            created_count: 0,
            eviction_count: 0,
        }
    }

    /// 获取精确匹配的闲置 surface，未命中时调用创建函数。
    pub(crate) fn acquire(
        &mut self,
        render_size: [u32; 2],
        config: InkSurfaceConfig,
        create: impl FnOnce() -> Result<Surface, AppError>,
    ) -> Result<Surface, AppError> {
        if let Some(index) = self
            .idle_surfaces
            .iter()
            .position(|entry| entry.render_size == render_size && entry.config == config)
        {
            let mut entry = self.idle_surfaces.swap_remove(index);
            self.estimated_bytes = self.estimated_bytes.saturating_sub(entry.estimated_bytes);
            self.reused_count += 1;
            entry.surface.canvas().clear(Color::TRANSPARENT);
            return Ok(entry.surface);
        }

        self.created_count += 1;
        create()
    }

    /// 把一个应用自有的离屏 surface 归还池中，超限时淘汰最久未使用项。
    pub(crate) fn release(
        &mut self,
        surface: Surface,
        render_size: [u32; 2],
        config: InkSurfaceConfig,
    ) {
        self.release_at(surface, render_size, config, Instant::now());
    }

    /// 清理闲置时间达到指定阈值的资源。
    pub(crate) fn gc(&mut self, timeout: Duration) {
        self.gc_at(Instant::now(), timeout);
    }

    /// 立即释放池中全部闲置资源。
    pub(crate) fn clear(&mut self) {
        self.eviction_count += self.idle_surfaces.len() as u64;
        self.idle_surfaces.clear();
        self.estimated_bytes = 0;
    }

    /// 返回资源池容量、命中和淘汰统计。
    pub(crate) fn stats(&self) -> SurfacePoolStats {
        SurfacePoolStats {
            idle_count: self.idle_surfaces.len(),
            estimated_bytes: self.estimated_bytes,
            reused_count: self.reused_count,
            created_count: self.created_count,
            eviction_count: self.eviction_count,
        }
    }

    /// 使用指定时间戳归还 surface，供生产逻辑和确定性测试复用。
    fn release_at(
        &mut self,
        mut surface: Surface,
        render_size: [u32; 2],
        config: InkSurfaceConfig,
        now: Instant,
    ) {
        let estimated_bytes = estimate_surface_bytes(render_size, config);
        if self.max_entries == 0 || estimated_bytes > self.max_estimated_bytes {
            return;
        }
        surface.canvas().clear(Color::TRANSPARENT);
        while self.idle_surfaces.len() >= self.max_entries
            || self.estimated_bytes.saturating_add(estimated_bytes) > self.max_estimated_bytes
        {
            if !self.evict_oldest() {
                break;
            }
        }
        if self.idle_surfaces.len() < self.max_entries
            && self.estimated_bytes.saturating_add(estimated_bytes) <= self.max_estimated_bytes
        {
            self.estimated_bytes += estimated_bytes;
            self.idle_surfaces.push(PooledSurface {
                surface,
                render_size,
                config,
                estimated_bytes,
                last_used: now,
            });
        }
    }

    /// 使用显式当前时间清理过期条目。
    fn gc_at(&mut self, now: Instant, timeout: Duration) {
        let mut index = 0;
        while index < self.idle_surfaces.len() {
            if now.saturating_duration_since(self.idle_surfaces[index].last_used) >= timeout {
                let entry = self.idle_surfaces.swap_remove(index);
                self.estimated_bytes = self.estimated_bytes.saturating_sub(entry.estimated_bytes);
                self.eviction_count += 1;
            } else {
                index += 1;
            }
        }
    }

    /// 淘汰最近最少使用的单个条目。
    fn evict_oldest(&mut self) -> bool {
        let Some(index) = self
            .idle_surfaces
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(index, _)| index)
        else {
            return false;
        };
        let entry = self.idle_surfaces.swap_remove(index);
        self.estimated_bytes = self.estimated_bytes.saturating_sub(entry.estimated_bytes);
        self.eviction_count += 1;
        true
    }
}

/// 保守估算 surface 的颜色缓冲和 MSAA 样本占用。
pub(crate) fn estimate_surface_bytes(render_size: [u32; 2], config: InkSurfaceConfig) -> usize {
    let sample_multiplier = if config.sample_count == 0 {
        1
    } else {
        config.sample_count.saturating_add(1)
    };
    (render_size[0] as usize)
        .saturating_mul(render_size[1] as usize)
        .saturating_mul(BYTES_PER_PIXEL)
        .saturating_mul(sample_multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::InkAntialiasingMode;

    /// 创建测试用 raster surface。
    fn raster_surface(size: [u32; 2]) -> Surface {
        skia_safe::surfaces::raster_n32_premul((size[0] as i32, size[1] as i32))
            .expect("测试 surface 应创建成功")
    }

    /// 返回关闭抗锯齿模式的 surface 配置。
    fn off_config() -> InkSurfaceConfig {
        InkSurfaceConfig::for_mode(InkAntialiasingMode::Off)
    }

    /// 验证尺寸与配置精确匹配时复用已归还 surface。
    #[test]
    fn exact_match_reuses_surface() {
        let mut pool = SurfacePool::new(5, 5 * 1024 * 1024);
        pool.release(raster_surface([64, 64]), [64, 64], off_config());

        let surface = pool
            .acquire([64, 64], off_config(), || {
                panic!("精确匹配不应创建新 surface")
            })
            .expect("精确匹配应返回 surface");

        assert_eq!((surface.width(), surface.height()), (64, 64));
        assert_eq!(pool.stats().reused_count, 1);
        assert_eq!(pool.stats().idle_count, 0);
    }

    /// 验证近似但不相同的尺寸不会破坏固定像素边界契约。
    #[test]
    fn similar_size_does_not_match() {
        let mut pool = SurfacePool::new(5, 5 * 1024 * 1024);
        pool.release(raster_surface([64, 64]), [64, 64], off_config());

        let surface = pool
            .acquire([68, 68], off_config(), || Ok(raster_surface([68, 68])))
            .expect("不匹配时应创建 surface");

        assert_eq!((surface.width(), surface.height()), (68, 68));
        assert_eq!(pool.stats().created_count, 1);
        assert_eq!(pool.stats().idle_count, 1);
    }

    /// 验证条目数限制按最近最少使用顺序淘汰。
    #[test]
    fn entry_limit_evicts_oldest_surface() {
        let mut pool = SurfacePool::new(2, 5 * 1024 * 1024);
        let start = Instant::now();
        pool.release_at(raster_surface([32, 32]), [32, 32], off_config(), start);
        pool.release_at(
            raster_surface([48, 48]),
            [48, 48],
            off_config(),
            start + Duration::from_secs(1),
        );
        pool.release_at(
            raster_surface([64, 64]),
            [64, 64],
            off_config(),
            start + Duration::from_secs(2),
        );

        assert_eq!(pool.stats().idle_count, 2);
        assert_eq!(pool.stats().eviction_count, 1);
        assert!(
            pool.idle_surfaces
                .iter()
                .all(|entry| entry.render_size != [32, 32])
        );
        assert!(
            pool.idle_surfaces
                .iter()
                .any(|entry| entry.render_size == [48, 48])
        );
        assert!(
            pool.idle_surfaces
                .iter()
                .any(|entry| entry.render_size == [64, 64])
        );
    }

    /// 验证估算内存预算会阻止池容量超过 5MB。
    #[test]
    fn memory_limit_is_enforced() {
        let mut pool = SurfacePool::new(5, 5 * 1024 * 1024);
        pool.release(raster_surface([768, 768]), [768, 768], off_config());
        pool.release(raster_surface([768, 768]), [768, 768], off_config());
        pool.release(raster_surface([768, 768]), [768, 768], off_config());

        assert_eq!(pool.stats().idle_count, 2);
        assert!(pool.stats().estimated_bytes <= 5 * 1024 * 1024);
    }

    /// 验证垃圾回收只移除达到闲置阈值的条目。
    #[test]
    fn gc_removes_expired_surfaces() {
        let mut pool = SurfacePool::new(5, 5 * 1024 * 1024);
        let start = Instant::now();
        pool.release_at(raster_surface([32, 32]), [32, 32], off_config(), start);
        pool.release_at(
            raster_surface([48, 48]),
            [48, 48],
            off_config(),
            start + Duration::from_secs(20),
        );

        pool.gc_at(start + Duration::from_secs(31), Duration::from_secs(30));

        assert_eq!(pool.stats().idle_count, 1);
        assert_eq!(pool.idle_surfaces[0].render_size, [48, 48]);
    }
}
