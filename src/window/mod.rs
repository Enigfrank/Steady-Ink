mod d3d_context;

pub(crate) use d3d_context::SWAP_CHAIN_BUFFER_COUNT;
pub use d3d_context::{D3DWindowContext, DockSide, GraphicsDiagnostics, IdleWindowView};
#[cfg(test)]
pub(crate) use d3d_context::{
    IDLE_HEIGHT_POINTS, IDLE_WIDTH_POINTS, QUICK_SETTINGS_HEIGHT_POINTS,
    QUICK_SETTINGS_WIDTH_POINTS,
};
