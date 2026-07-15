use crate::{
    ink::InkDocument,
    slideshow::{SlidePage, SlideShowKey, SlideShowSession},
};

/// 顶层界面与输入路由使用的应用模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// 应用状态机，集中维护普通墨迹、放映会话和断线抑制规则。
#[derive(Debug, Clone)]
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
    ) -> bool {
        if self.mode != AppMode::SlideShowConnectionLost {
            return false;
        }
        let Some(session) = self.slideshow_session.as_mut() else {
            return false;
        };
        if session.key() != show_key {
            return false;
        }

        session.switch_page(current_page);
        self.mode = AppMode::SlideShowAnnotatingExpanded;
        true
    }

    /// 处理 COM 页切换事件并保存、恢复对应位置墨迹。
    pub fn change_slide(&mut self, show_key: &SlideShowKey, target_page: SlidePage) -> bool {
        let Some(session) = self.slideshow_session.as_mut() else {
            return false;
        };
        if session.key() != show_key {
            return false;
        }
        session.switch_page(target_page);
        true
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ink::PageKey,
        slideshow::{PresentationApplication, SlideShowKey},
    };

    /// 创建状态机测试使用的放映会话。
    fn session(window_id: i64) -> SlideShowSession {
        let key = SlideShowKey::new(PresentationApplication::PowerPoint, "deck", window_id);
        let page = SlidePage::new(PageKey::new(1).expect("有效页键"), Some(1), Some(10));
        SlideShowSession::new(key, page)
    }

    /// 验证普通批注退出后文档被立即清空。
    #[test]
    fn normal_annotation_exit_clears_document() {
        let mut state = AppState::default();
        assert!(state.enter_normal_annotation());
        state.normal_document_mut().append_draw_stroke(
            vec![crate::ink::CanvasPoint::new(1.0, 1.0)],
            crate::ink::InkColor::Red,
            crate::ink::PenWidth::Px8,
        );

        assert!(state.exit_normal_annotation());
        assert_eq!(state.mode(), AppMode::IdleFloatingToolbar);
        assert!(state.normal_document.has_no_history());
    }

    /// 验证连接中断会禁用放映控制并强制进入降级态。
    #[test]
    fn connection_loss_disables_slideshow_controls() {
        let mut state = AppState::default();
        assert!(state.start_slideshow(session(7)));
        assert!(state.collapse_slideshow_toolbar());
        assert!(state.lose_slideshow_connection());
        assert_eq!(state.mode(), AppMode::SlideShowConnectionLost);
        assert!(!state.slideshow_controls_enabled());
    }

    /// 验证教师退出降级态后，同一场放映不会因重连再次进入批注。
    #[test]
    fn dismissed_show_is_suppressed_until_it_ends() {
        let mut state = AppState::default();
        let first_session = session(7);
        let key = first_session.key().clone();
        assert!(state.start_slideshow(first_session));
        assert!(state.lose_slideshow_connection());
        assert!(state.dismiss_disconnected_slideshow());
        assert!(!state.start_slideshow(session(7)));

        assert!(state.end_slideshow(&key));
        assert!(state.start_slideshow(session(7)));
    }

    /// 验证其他放映实例的翻页事件不会污染当前会话。
    #[test]
    fn slide_change_requires_matching_show_key() {
        let mut state = AppState::default();
        assert!(state.start_slideshow(session(7)));
        let other_key = SlideShowKey::new(PresentationApplication::PowerPoint, "deck", 8);
        let target = SlidePage::new(PageKey::new(2).expect("有效页键"), Some(2), Some(10));

        assert!(!state.change_slide(&other_key, target));
        assert_eq!(
            state
                .slideshow_session()
                .expect("放映会话仍存在")
                .current_page()
                .key
                .show_position(),
            1
        );
    }
}
