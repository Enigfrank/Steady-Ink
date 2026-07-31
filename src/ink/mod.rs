mod batch_drawer;
mod document;
mod model;
mod natural_taper;
mod page_store;
pub(crate) mod renderer;
mod spatial_index;
mod stroke_geometry;

pub(crate) use batch_drawer::BatchDrawer;
pub use document::InkDocument;
pub use model::{
    CanvasPoint, ClearOperation, DrawStroke, DrawStrokeShape, EraseSample, EraseStroke, EraserSize,
    InkBounds, InkColor, InkOperation, InkTool, OperationId, PenWidth, VariableStrokePoint,
};
pub(crate) use natural_taper::NaturalStrokeBuilder;
pub use page_store::{PageInkEntry, PageInkStore, PageKey};
pub(crate) use renderer::draw_active_preview;
pub use renderer::{ActiveInkPreview, InkRenderCache, InkSyncKind, OwnedActiveInkPreview};
pub(crate) use spatial_index::InkSpatialIndex;
