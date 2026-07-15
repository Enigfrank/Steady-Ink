mod gl_context;

pub use gl_context::{DockSide, GlDiagnostics, GlWindowContext, IdleWindowView};
#[cfg(test)]
pub(crate) use gl_context::{
    IDLE_HEIGHT_POINTS, IDLE_WIDTH_POINTS, QUICK_SETTINGS_HEIGHT_POINTS,
    QUICK_SETTINGS_WIDTH_POINTS,
};
