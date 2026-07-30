use std::mem;

use serde::{Deserialize, Serialize};

use crate::ink::{InkDocument, PageInkEntry, PageInkStore, PageKey};

/// 支持联动的演示应用类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PresentationApplication {
    PowerPoint,
    Wps,
}

/// 唯一标识一次活动放映，用于断线恢复和手动退出抑制。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SlideShowKey {
    pub application: PresentationApplication,
    pub presentation_id: String,
    pub window_id: i64,
}

impl SlideShowKey {
    /// 创建一个由 COM 适配层确认的放映标识。
    pub fn new(
        application: PresentationApplication,
        presentation_id: impl Into<String>,
        window_id: i64,
    ) -> Self {
        Self {
            application,
            presentation_id: presentation_id.into(),
            window_id,
        }
    }
}

/// COM 报告的当前放映位置和可选可靠页数信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlidePage {
    pub key: PageKey,
    pub stable_slide_id: Option<i64>,
    pub total_pages: Option<u32>,
}

impl SlidePage {
    /// 创建一份经过 COM 适配层验证的放映页快照。
    pub const fn new(key: PageKey, stable_slide_id: Option<i64>, total_pages: Option<u32>) -> Self {
        Self {
            key,
            stable_slide_id,
            total_pages,
        }
    }

    /// 仅在当前页和总页数都可靠时返回页码元组。
    pub const fn reliable_page_numbers(self) -> Option<(u32, u32)> {
        let current = self.key.show_position();
        match self.total_pages {
            Some(total) if total > 0 && current <= total => Some((current, total)),
            _ => None,
        }
    }
}

/// 一次 PowerPoint/WPS 放映的内存墨迹会话。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlideShowSession {
    key: SlideShowKey,
    current_page: SlidePage,
    current_document: InkDocument,
    page_store: PageInkStore,
}

/// 放映页快照应用到会话后的精确结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSwitchOutcome {
    Unchanged,
    MetadataUpdated,
    PageChanged,
}

impl SlideShowSession {
    /// 从 COM 确认的放映标识和当前页创建新会话。
    pub fn new(key: SlideShowKey, current_page: SlidePage) -> Self {
        Self {
            key,
            current_page,
            current_document: InkDocument::new(),
            page_store: PageInkStore::new(),
        }
    }

    /// 返回当前放映的稳定会话标识。
    pub fn key(&self) -> &SlideShowKey {
        &self.key
    }

    /// 返回当前页快照。
    pub const fn current_page(&self) -> SlidePage {
        self.current_page
    }

    /// 返回当前活动页的只读墨迹文档。
    pub const fn current_document(&self) -> &InkDocument {
        &self.current_document
    }

    /// 返回当前活动页的可变墨迹文档。
    pub fn current_document_mut(&mut self) -> &mut InkDocument {
        &mut self.current_document
    }

    /// 应用页快照，仅在页键真实变化时保存和恢复逐页墨迹。
    pub fn switch_page(&mut self, target_page: SlidePage) -> PageSwitchOutcome {
        if self.current_page == target_page {
            return PageSwitchOutcome::Unchanged;
        }
        if self.current_page.key == target_page.key {
            self.current_page = target_page;
            return PageSwitchOutcome::MetadataUpdated;
        }

        let leaving_document = mem::take(&mut self.current_document);
        self.page_store.save(
            self.current_page.key,
            PageInkEntry {
                stable_slide_id: self.current_page.stable_slide_id,
                document: leaving_document,
            },
        );

        let entering_entry = self.page_store.take(target_page.key);
        self.current_document = entering_entry.document;
        self.current_page = target_page;
        PageSwitchOutcome::PageChanged
    }

    /// 返回当前保存在非活动位置上的文档数量。
    pub fn saved_page_count(&self) -> usize {
        self.page_store.saved_page_count()
    }

    /// 校验活动页、活动文档和全部非活动页的恢复约束。
    pub(crate) fn validate_recovery(&self) -> Result<(), String> {
        if !self.current_page.key.is_valid() {
            return Err("活动放映页键必须大于零".to_owned());
        }
        if self.page_store.contains(self.current_page.key) {
            return Err("活动放映页不能同时存在于非活动页存储".to_owned());
        }
        self.current_document.validate_recovery()?;
        self.page_store.validate_recovery()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::{CanvasPoint, InkColor, PenWidth};

    /// 创建页切换测试使用的固定放映会话。
    fn session_fixture() -> SlideShowSession {
        SlideShowSession::new(
            SlideShowKey::new(PresentationApplication::PowerPoint, "deck", 1),
            SlidePage::new(PageKey::new(1).expect("测试页键有效"), Some(10), Some(2)),
        )
    }

    /// 验证重复快照和同页元数据变化不会交换活动文档。
    #[test]
    fn same_page_distinguishes_unchanged_and_metadata_update() {
        let mut session = session_fixture();
        let original = session.current_page();

        assert_eq!(session.switch_page(original), PageSwitchOutcome::Unchanged);
        assert_eq!(session.saved_page_count(), 0);

        let updated = SlidePage::new(original.key, original.stable_slide_id, Some(3));
        assert_eq!(
            session.switch_page(updated),
            PageSwitchOutcome::MetadataUpdated
        );
        assert_eq!(session.current_page(), updated);
        assert_eq!(session.saved_page_count(), 0);
    }

    /// 验证页一到页二再返回页一时恢复原文档且不串页。
    #[test]
    fn page_round_trip_restores_each_document() {
        let mut session = session_fixture();
        session.current_document_mut().append_draw_stroke(
            vec![CanvasPoint::new(1.0, 1.0), CanvasPoint::new(2.0, 2.0)],
            InkColor::Red,
            PenWidth::Px4,
        );
        let page_one_document = session.current_document().clone();
        let page_two = SlidePage::new(PageKey::new(2).expect("测试页键有效"), Some(20), Some(2));

        assert_eq!(
            session.switch_page(page_two),
            PageSwitchOutcome::PageChanged
        );
        assert!(session.current_document().operations().is_empty());
        session.current_document_mut().append_draw_stroke(
            vec![CanvasPoint::new(3.0, 3.0), CanvasPoint::new(4.0, 4.0)],
            InkColor::Blue,
            PenWidth::Px8,
        );
        let page_two_document = session.current_document().clone();

        assert_eq!(
            session.switch_page(SlidePage::new(
                PageKey::new(1).expect("测试页键有效"),
                Some(10),
                Some(2),
            )),
            PageSwitchOutcome::PageChanged
        );
        assert_eq!(session.current_document(), &page_one_document);
        assert_eq!(session.saved_page_count(), 1);

        session.switch_page(page_two);
        assert_eq!(session.current_document(), &page_two_document);
    }

    /// 验证恢复数据不能为同一页同时保存活动和非活动文档。
    #[test]
    fn recovery_rejects_duplicate_active_page() {
        let key = PageKey::new(1).expect("测试页键有效");
        let page = SlidePage::new(key, Some(10), Some(2));
        let mut session = SlideShowSession::new(
            SlideShowKey::new(PresentationApplication::PowerPoint, "deck", 1),
            page,
        );
        session.page_store.save(key, PageInkEntry::default());

        assert!(session.validate_recovery().is_err());
    }
}
