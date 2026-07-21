mod document;
mod model;
mod page_store;
pub(crate) mod renderer;
mod speed_taper;
mod stroke_geometry;

pub use document::InkDocument;
pub use model::{
    CanvasPoint, ClearOperation, DrawStroke, DrawStrokeShape, EraseSample, EraseStroke, EraserSize,
    InkBounds, InkColor, InkOperation, InkTool, OperationId, PenWidth, VariableStrokePoint,
};
pub use page_store::{PageInkEntry, PageInkStore, PageKey};
pub use renderer::{ActiveInkPreview, InkRenderCache};
pub(crate) use renderer::{
    InkPreviewCache, InkSurfaceConfig, active_preview_bounds, draw_active_preview,
    draw_image_rect_logical, preview_replaces_region,
};
pub(crate) use speed_taper::SpeedStrokeBuilder;
