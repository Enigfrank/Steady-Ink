use std::collections::HashMap;

use super::InkDocument;

/// 单次放映中的位置键；同一幻灯片在自定义放映中重复出现时位置不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageKey(u32);

impl PageKey {
    /// 从 COM 报告的 1 基放映位置创建页键；零值被视为不可靠。
    pub const fn new(show_position: u32) -> Option<Self> {
        if show_position == 0 {
            None
        } else {
            Some(Self(show_position))
        }
    }

    /// 返回 COM 放映中的 1 基位置。
    pub const fn show_position(self) -> u32 {
        self.0
    }
}

/// 某个放映位置的墨迹文档和辅助幻灯片标识。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageInkEntry {
    pub stable_slide_id: Option<i64>,
    pub document: InkDocument,
}

/// 仅在一次放映会话内存在的逐页墨迹存储。
#[derive(Debug, Clone, Default)]
pub struct PageInkStore {
    pages: HashMap<PageKey, PageInkEntry>,
}

impl PageInkStore {
    /// 创建一个空的会话内页存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 保存离开的放映位置，并覆盖该位置之前的内存版本。
    pub fn save(&mut self, key: PageKey, entry: PageInkEntry) {
        self.pages.insert(key, entry);
    }

    /// 取出进入位置的文档；首次进入返回空文档。
    pub fn take(&mut self, key: PageKey) -> PageInkEntry {
        self.pages.remove(&key).unwrap_or_default()
    }

    /// 返回当前已保存的非活动放映位置数量。
    pub fn saved_page_count(&self) -> usize {
        self.pages.len()
    }

    /// 清空本次放映的全部逐页墨迹。
    pub fn clear(&mut self) {
        self.pages.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::{CanvasPoint, InkColor, PenWidth};

    /// 验证两个放映位置保存的墨迹文档互不串页。
    #[test]
    fn page_documents_are_isolated_by_show_position() {
        let first_page = PageKey::new(1).expect("有效页键");
        let second_page = PageKey::new(2).expect("有效页键");
        let mut first_document = InkDocument::new();
        first_document.append_draw_stroke(
            vec![CanvasPoint::new(1.0, 1.0)],
            InkColor::Red,
            PenWidth::Px8,
        );

        let mut store = PageInkStore::new();
        store.save(
            first_page,
            PageInkEntry {
                stable_slide_id: Some(100),
                document: first_document,
            },
        );

        assert!(store.take(second_page).document.has_no_history());
        assert_eq!(store.take(first_page).stable_slide_id, Some(100));
    }
}
