mod d3d_context;
mod slideshow_hit_test;
mod slideshow_ui_window;

pub(crate) use d3d_context::SWAP_CHAIN_BUFFER_COUNT;
pub(crate) use d3d_context::WindowPlacement;
pub use d3d_context::{
    D3DRenderContext, D3DRenderTarget, D3DWindowContext, DockSide, GraphicsDiagnostics,
    IdleWindowView,
};
#[cfg(test)]
pub(crate) use d3d_context::{QUICK_SETTINGS_HEIGHT_POINTS, QUICK_SETTINGS_WIDTH_POINTS};
pub(crate) use slideshow_hit_test::PhysicalHitRect;
pub use slideshow_ui_window::SlideshowUiWindow;
