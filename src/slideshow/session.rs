use std::mem;

use crate::ink::{InkDocument, PageInkEntry, PageInkStore, PageKey};

/// 支持联动的演示应用类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresentationApplication {
    PowerPoint,
    Wps,
}

/// 唯一标识一次活动放映，用于断线恢复和手动退出抑制。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
pub struct SlideShowSession {
    key: SlideShowKey,
    current_page: SlidePage,
    current_document: InkDocument,
    page_store: PageInkStore,
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

    /// 保存离开页的墨迹并恢复目标放映位置的会话内墨迹。
    pub fn switch_page(&mut self, target_page: SlidePage) {
        if self.current_page.key == target_page.key {
            self.current_page = target_page;
            return;
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
    }

    /// 返回当前保存在非活动位置上的文档数量。
    pub fn saved_page_count(&self) -> usize {
        self.page_store.saved_page_count()
    }
}
