use crate::{
    ink::InkDocument,
    slideshow::{PageSwitchOutcome, SlidePage, SlideShowKey, SlideShowSession},
};

/// 顶层界面与输入路由使用的应用模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppMode {
    IdleFloatingToolbar,
    NormalAnnotating,
    SlideShowAnnotatingExpanded,
    SlideShowAnnotatingCollapsed,
    SlideShowConnectionLost,
}

impl AppMode {
    /// 返回当前模式是否允许在透明画布上书写或擦除。
    pub const fn accepts_ink_input(self) -> bool {
        matches!(
            self,
            Self::NormalAnnotating
                | Self::SlideShowAnnotatingExpanded
                | Self::SlideShowAnnotatingCollapsed
                | Self::SlideShowConnectionLost
        )
    }

    /// 返回当前模式是否属于已建立的放映批注会话。
    pub const fn is_slideshow(self) -> bool {
        matches!(
            self,
            Self::SlideShowAnnotatingExpanded
                | Self::SlideShowAnnotatingCollapsed
                | Self::SlideShowConnectionLost
        )
    }
}

/// 当前放映会话的瞬时画布输入模式，不进入恢复文件或用户设置。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SlideshowInputMode {
    #[default]
    Ink,
    Mouse,
}

impl SlideshowInputMode {
    /// 返回当前放映画布是否允许创建或擦除软件墨迹。
    pub const fn accepts_ink_input(self) -> bool {
        matches!(self, Self::Ink)
    }
}

/// 应用状态机，集中维护普通墨迹、放映会话和断线抑制规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    mode: AppMode,
    normal_document: InkDocument,
    slideshow_session: Option<SlideShowSession>,
    suppressed_show_key: Option<SlideShowKey>,
}

impl Default for AppState {
    /// 创建启动后的悬浮工具栏状态。
    fn default() -> Self {
        Self {
            mode: AppMode::IdleFloatingToolbar,
            normal_document: InkDocument::new(),
            slideshow_session: None,
            suppressed_show_key: None,
        }
    }
}

impl AppState {
    /// 返回当前顶层模式。
    pub const fn mode(&self) -> AppMode {
        self.mode
    }

    /// 返回普通批注文档的可变引用。
    pub fn normal_document_mut(&mut self) -> &mut InkDocument {
        &mut self.normal_document
    }

    /// 返回普通批注文档的只读引用。
    pub const fn normal_document(&self) -> &InkDocument {
        &self.normal_document
    }

    /// 返回当前模式对应的活动墨迹文档；悬浮工具栏模式没有画布文档。
    pub fn active_document(&self) -> Option<&InkDocument> {
        match self.mode {
            AppMode::NormalAnnotating => Some(&self.normal_document),
            mode if mode.is_slideshow() => self
                .slideshow_session
                .as_ref()
                .map(SlideShowSession::current_document),
            _ => None,
        }
    }

    /// 返回当前模式对应的可变活动墨迹文档。
    pub fn active_document_mut(&mut self) -> Option<&mut InkDocument> {
        match self.mode {
            AppMode::NormalAnnotating => Some(&mut self.normal_document),
            mode if mode.is_slideshow() => self
                .slideshow_session
                .as_mut()
                .map(SlideShowSession::current_document_mut),
            _ => None,
        }
    }

    /// 返回当前放映会话。
    pub const fn slideshow_session(&self) -> Option<&SlideShowSession> {
        self.slideshow_session.as_ref()
    }

    /// 返回当前放映会话的可变引用。
    pub fn slideshow_session_mut(&mut self) -> Option<&mut SlideShowSession> {
        self.slideshow_session.as_mut()
    }

    /// 从悬浮工具栏进入普通批注模式。
    pub fn enter_normal_annotation(&mut self) -> bool {
        if self.mode != AppMode::IdleFloatingToolbar {
            return false;
        }
        self.mode = AppMode::NormalAnnotating;
        true
    }

    /// 退出普通批注并按产品约定清空普通墨迹。
    pub fn exit_normal_annotation(&mut self) -> bool {
        if self.mode != AppMode::NormalAnnotating {
            return false;
        }
        self.normal_document = InkDocument::new();
        self.mode = AppMode::IdleFloatingToolbar;
        true
    }

    /// 接受 COM 确认的放映开始事件；被抑制的同一场放映不会重新进入批注。
    pub fn start_slideshow(&mut self, session: SlideShowSession) -> bool {
        if self.suppressed_show_key.as_ref() == Some(session.key()) {
            return false;
        }
        if self.suppressed_show_key.is_some() {
            self.suppressed_show_key = None;
        }

        self.normal_document = InkDocument::new();
        self.slideshow_session = Some(session);
        self.mode = AppMode::SlideShowAnnotatingExpanded;
        true
    }

    /// 收缩放映底部工具栏主体，同时保留两侧翻页控件。
    pub fn collapse_slideshow_toolbar(&mut self) -> bool {
        if self.mode != AppMode::SlideShowAnnotatingExpanded {
            return false;
        }
        self.mode = AppMode::SlideShowAnnotatingCollapsed;
        true
    }

    /// 展开放映底部工具栏主体。
    pub fn expand_slideshow_toolbar(&mut self) -> bool {
        if self.mode != AppMode::SlideShowAnnotatingCollapsed {
            return false;
        }
        self.mode = AppMode::SlideShowAnnotatingExpanded;
        true
    }

    /// 进入 COM 连接中断降级态，并强制采用展开布局。
    pub fn lose_slideshow_connection(&mut self) -> bool {
        if !matches!(
            self.mode,
            AppMode::SlideShowAnnotatingExpanded | AppMode::SlideShowAnnotatingCollapsed
        ) {
            return false;
        }
        self.mode = AppMode::SlideShowConnectionLost;
        true
    }

    /// 在 COM 恢复且仍为同一场放映时同步当前页并恢复展开态。
    pub fn restore_slideshow_connection(
        &mut self,
        show_key: &SlideShowKey,
        current_page: SlidePage,
    ) -> Option<PageSwitchOutcome> {
        if self.mode != AppMode::SlideShowConnectionLost {
            return None;
        }
        let session = self.slideshow_session.as_mut()?;
        if session.key() != show_key {
            return None;
        }

        let outcome = session.switch_page(current_page);
        self.mode = AppMode::SlideShowAnnotatingExpanded;
        Some(outcome)
    }

    /// 处理 COM 页切换事件并保存、恢复对应位置墨迹。
    pub fn change_slide(
        &mut self,
        show_key: &SlideShowKey,
        target_page: SlidePage,
    ) -> Option<PageSwitchOutcome> {
        let session = self.slideshow_session.as_mut()?;
        if session.key() != show_key {
            return None;
        }
        Some(session.switch_page(target_page))
    }

    /// 处理 COM 确认的放映结束，清空会话和对应抑制标记。
    pub fn end_slideshow(&mut self, show_key: &SlideShowKey) -> bool {
        let session_matches = self
            .slideshow_session
            .as_ref()
            .is_some_and(|session| session.key() == show_key);
        let suppression_matches = self.suppressed_show_key.as_ref() == Some(show_key);
        if !session_matches && !suppression_matches {
            return false;
        }

        if session_matches {
            self.slideshow_session = None;
            self.mode = AppMode::IdleFloatingToolbar;
        }
        if suppression_matches {
            self.suppressed_show_key = None;
        }
        true
    }

    /// 在连接中断确认框中退出本地批注，并抑制同一场放映自动重入。
    pub fn dismiss_disconnected_slideshow(&mut self) -> bool {
        if self.mode != AppMode::SlideShowConnectionLost {
            return false;
        }
        let Some(session) = self.slideshow_session.take() else {
            return false;
        };

        self.suppressed_show_key = Some(session.key().clone());
        self.mode = AppMode::IdleFloatingToolbar;
        true
    }

    /// 返回放映控制是否可用；连接中断时 COM 和按键模拟都必须禁用。
    pub fn slideshow_controls_enabled(&self) -> bool {
        matches!(
            self.mode,
            AppMode::SlideShowAnnotatingExpanded | AppMode::SlideShowAnnotatingCollapsed
        ) && self.slideshow_session.is_some()
    }

    /// 校验恢复后的文档与顶层模式关系，拒绝会破坏状态机的不一致数据。
    pub(crate) fn validate_recovery(&self) -> Result<(), String> {
        self.normal_document.validate_recovery()?;
        if let Some(session) = &self.slideshow_session {
            session.validate_recovery()?;
        }
        if self.mode.is_slideshow() != self.slideshow_session.is_some() {
            return Err("应用模式与放映会话存在状态不一致".to_owned());
        }
        if self.slideshow_session.is_some() && self.suppressed_show_key.is_some() {
            return Err("活动放映会话不能同时持有重入抑制键".to_owned());
        }
        Ok(())
    }
}
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ink::PageKey, slideshow::PresentationApplication};

    /// 验证恢复数据不能同时表示活动放映和已抑制放映。
    #[test]
    fn recovery_rejects_active_and_suppressed_slideshow() {
        let key = SlideShowKey::new(PresentationApplication::PowerPoint, "deck", 1);
        let page = SlidePage::new(PageKey::new(1).expect("测试页键有效"), Some(10), Some(2));
        let mut state = AppState::default();
        assert!(state.start_slideshow(SlideShowSession::new(key.clone(), page)));
        state.suppressed_show_key = Some(key);

        assert!(state.validate_recovery().is_err());
    }

    /// 验证真实换页和重复快照都不会改变放映工具栏展开状态。
    #[test]
    fn page_updates_preserve_slideshow_toolbar_state() {
        let key = SlideShowKey::new(PresentationApplication::PowerPoint, "deck", 1);
        let first = SlidePage::new(PageKey::new(1).expect("测试页键有效"), Some(10), Some(2));
        let second = SlidePage::new(PageKey::new(2).expect("测试页键有效"), Some(20), Some(2));
        let mut state = AppState::default();
        assert!(state.start_slideshow(SlideShowSession::new(key.clone(), first)));
        assert!(state.collapse_slideshow_toolbar());

        assert_eq!(
            state.change_slide(&key, first),
            Some(PageSwitchOutcome::Unchanged)
        );
        assert_eq!(state.mode(), AppMode::SlideShowAnnotatingCollapsed);
        assert_eq!(
            state.change_slide(&key, second),
            Some(PageSwitchOutcome::PageChanged)
        );
        assert_eq!(state.mode(), AppMode::SlideShowAnnotatingCollapsed);
    }
}
